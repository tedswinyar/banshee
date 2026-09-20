// census — the 5-minute tier: who is running, how long they have been running,
// and what it is costing.
//
// This is the tier that answers the question the 2026-08-04 incident exposed:
// the machine hit load 76 with swap at 99%, and the cause was not the processes
// at the top of `top` — it was 17 multi-day agent sessions and ~500 MCP helper
// processes nobody could see. The cheap tier would have shown the symptom; only
// this tier names the cause.
//
// Subprocesses live here and nowhere else (ADR-0007): two `ps` calls, one `tmux`
// call, one `log show` call (honest jetsam kills, `banshee-2dq`), and `lsof`
// ONLY for sessions already flagged stale.

pub mod config;
pub mod exec;
pub mod parse;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use crate::Result;
use config::CensusConfig;
use exec::CommandRunner;
use parse::ProcRow;

/// An interactive agent CLI session — one a person is sitting in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub pid: u32,
    pub program: String,
    pub rss_bytes: u64,
    pub age_secs: u64,
    pub age_days: u64,
    pub cpu_secs: f64,
    pub tty: String,
    pub is_stale: bool,
    /// Present only for STALE sessions: `lsof` is not run for the rest.
    pub cwd: Option<String>,
}

/// IDE-spawned agent helpers, rolled up rather than listed.
///
/// `perf-scan`'s comment earns its place: without this split the session list is
/// unusable. One measurement found dozens of subprocesses from a single agent CLI
/// burying the seven real sessions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelperRollup {
    pub count: u32,
    pub rss_bytes: u64,
}

/// Agent helper processes (MCP servers, language servers, parsers) and how many
/// of them have been orphaned by a dead parent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanCensus {
    pub total_count: u32,
    pub total_rss_bytes: u64,
    /// `ppid == 1`: the session that spawned them died and left them running.
    pub orphan_count: u32,
    pub orphan_rss_bytes: u64,
    /// Orphans grouped by program, descending — the actionable breakdown.
    pub orphans_by_program: Vec<ProgramCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramCount {
    pub program: String,
    pub count: u32,
    pub rss_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmuxSessionInfo {
    pub name: String,
    pub pane_command: String,
    /// `None` when the activity timestamp was unknown — which is NOT the same as
    /// "idle forever", and must never make a session look reapable.
    pub idle_days: Option<f64>,
    /// Mid-build or mid-test: never reap, whatever its age.
    pub is_busy: bool,
    /// Old enough to be a reap candidate. Requires a known idle age.
    pub is_stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppGroup {
    pub name: String,
    pub proc_count: u32,
    pub rss_bytes: u64,
}

/// A managed monitoring/security agent's real cost.
///
/// **Never sum `percent_of_one_core` across this list.** Use
/// `Census::monitor_total`. Two independent reasons, both measured on a real
/// machine:
///
/// 1. **Every group has its OWN denominator.** Each percentage divides by the
///    longest-lived process *in that group*, so a group of freshly-restarted
///    processes is a percentage of a few minutes while another is a percentage of
///    a day. Adding those is meaningless arithmetic. This applies always, and is
///    the larger effect: a real census summed to 59.9% while the honest total was
///    49.2% — a 10.7-point overstatement with no overlap involved at all.
/// 2. **Groups may overlap.** A pattern list can match the same process twice and
///    each group counts it, which is also what `perf-scan` does since it runs one
///    `grep` per name. Observed: one pid matching two configured patterns.
///
/// `monitor_total` fixes both by counting each process once against a single
/// global denominator.
///
/// **`percent_of_one_core` is noisy for short-lived processes.** It is
/// `Σ CPU ÷ max(lifetime)`, so a daemon restarted a minute ago shows its startup
/// burst as a large steady-state figure — measured: a helper 109 s old
/// reported 14.6% off 16 s of CPU. Any threshold on this must require a minimum
/// lifetime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorAgent {
    pub name: String,
    pub proc_count: u32,
    pub rss_bytes: u64,
    pub cpu_secs_total: f64,
    /// `Σ cumulative CPU × 100 ÷ max(lifetime)` — **not `%CPU`**, which is
    /// instantaneous and lies about daemons.
    pub percent_of_one_core: f64,
    /// Longest-lived matching process, in seconds. The denominator above, exposed
    /// so a consumer can refuse to band on a figure derived from a short life.
    pub longest_life_secs: u64,
}

/// The whole managed-agent population, counted ONCE per process.
///
/// The honest answer to "what are the managed agents costing me", and the number to
/// show a user. Summing the per-agent list instead double-counts every process
/// that matches more than one pattern.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorTotal {
    /// Distinct processes matching ANY configured pattern.
    pub proc_count: u32,
    pub rss_bytes: u64,
    pub cpu_secs_total: f64,
    /// Against ONE core. Divide by the core count for a share of the machine.
    pub percent_of_one_core: f64,
    pub longest_life_secs: u64,
}

/// One consumer behind the CPU/thermal who-line, measured as a RECENT rate.
///
/// Every other census figure is a snapshot; this one is a DELTA. `percent_of_one_core`
/// is `Σ(Δ cpu_secs) × 100 ÷ Δ wall_secs` across the two most recent censuses —
/// "what is burning CPU right now", not "what has burned the most CPU over its
/// life". The cumulative-CPU rule the memory-shaped who-line uses is right for the
/// managed-agents baseline (a daemon's lifetime average) and wrong here: a 30-hour agent
/// session at 46% live reads as 4% cumulative, and a desktop app that just spiked to
/// 108% has no lifetime figure the census ever collected (`banshee-aen`).
///
/// Because every consumer shares ONE denominator — the census interval, not a
/// per-group lifetime — these percentages, unlike `MonitorAgent::percent_of_one_core`,
/// MAY be summed. Each process is counted once, under a single label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuConsumer {
    /// The app-group name if the process matched one (so 115 Chrome helpers read
    /// as one `Google Chrome`), otherwise the program's `comm` basename — which is
    /// how desktop apps and build tools (`Microsoft Word`, `clippy-driver`,
    /// `WindowServer`) finally get named at all.
    pub name: String,
    /// Processes in the group that burned CPU over the interval.
    pub proc_count: u32,
    /// Recent rate, in percent of one core. See the type docs: summable.
    pub percent_of_one_core: f64,
}

/// The per-pid cumulative CPU seconds of ONE census cycle, kept in memory so the
/// NEXT cycle can difference against it. Never serialized — the differenced
/// result (`Census::cpu_consumers`) is what survives storage; this is scaffolding
/// the collector holds between cycles, the "keep the previous census's per-pid
/// times" the fix calls for (`banshee-aen`). A fresh daemon has none, so its first
/// census names CPU by the cumulative fallback until a second census exists.
#[derive(Debug, Clone)]
pub struct CpuSnapshot {
    pub at: DateTime<Utc>,
    pub cpu_by_pid: HashMap<u32, f64>,
}

/// How many consumers the census stores. The who-line cuts this to its own
/// `who_limit`; the census keeps a few more so a client asking a different
/// question is not starved by the finding's shorter list.
const CPU_CONSUMER_CAP: usize = 12;

/// The kernel-log predicate for jetsam kills (`banshee-2dq`). Every process the
/// kernel jetsams logs `memorystatus: killing…`; the reason token on the same
/// line is what distinguishes a real pressure kill from routine housekeeping.
const JETSAM_PREDICATE: &str =
    r#"process == "kernel" AND eventMessage CONTAINS "memorystatus: killing""#;

/// Upper bound on the jetsam-kill lookback window, in seconds. The window is
/// normally the gap since the previous census (so windows tile without overlap
/// or gap), but a laptop sleep or a stalled daemon can make that gap hours long
/// — and `log show`'s cost is proportional to the window length (a multi-hour
/// window takes over two minutes; a 5-minute one takes 2–4s). Capping the window
/// keeps the census-tier probe affordable AND stops a burst of kills logged
/// before a sleep from being attributed to the moment the machine woke — the
/// same "a gap is not evidence" mistake `held_secs` made (`a-monitor-s-held…`).
/// 15 minutes = 3× the default cadence: generous enough to rarely truncate a
/// genuine non-sleep gap, bounded enough that the probe stays cheap. No kills
/// happen while a machine is asleep, so truncating a sleep gap loses nothing.
const JETSAM_WINDOW_CAP_SECS: i64 = 900;

/// Count the HONEST pressure kills in `log show` output, excluding the routine
/// `idle-exit rf:low` daemon housekeeping that dominates a healthy machine
/// (~100–157 lines per 5 minutes, all of it `killing_idle_process … rf:low`).
///
/// Counting raw kill lines would make the dimension a false-positive generator
/// on every idle machine — the same shape as the `pages_free` mistake. The
/// honest signal is a kill flagged `rf:high`, `per-process-limit`, or
/// `vm-pageshortage`; the 2026-09-15 crisis kills were `idle-exit … rf:high` and
/// `killing_specific_process … (per-process-limit … rf:high)`.
fn count_honest_kills(log_output: &str) -> u64 {
    log_output
        .lines()
        .filter(|line| is_honest_pressure_kill(line))
        .count() as u64
}

/// A kill line counts iff it is a kill AND names a real-distress reason. The
/// explicit `idle-exit rf:low` exclusion is belt-and-suspenders over the
/// positive filter (rf:low is not rf:high), and documents the one shape that
/// MUST NOT count no matter how the positive set grows.
fn is_honest_pressure_kill(line: &str) -> bool {
    if !line.contains("memorystatus: killing") {
        return false;
    }
    if line.contains("idle-exit") && line.contains("rf:low") {
        return false;
    }
    line.contains("rf:high")
        || line.contains("per-process-limit")
        || line.contains("vm-pageshortage")
}

/// The current cycle's per-pid CPU snapshot, to difference next cycle.
pub fn cpu_snapshot(rows: &[ProcRow], at: DateTime<Utc>) -> CpuSnapshot {
    CpuSnapshot {
        at,
        cpu_by_pid: rows.iter().map(|r| (r.pid, r.cpu_secs)).collect(),
    }
}

/// The single label a process is grouped under for the recent-rate lens: its
/// app-group name if it matches one, else its `comm` basename. ONE label per
/// pid, so the rates never double-count the way summing overlapping monitor
/// patterns would.
fn cpu_label(r: &ProcRow, config: &CensusConfig) -> String {
    match config.app_groups_matching(&r.args).next() {
        Some(name) => name.to_string(),
        None => r.program().to_string(),
    }
}

/// Rank CPU consumers by their recent rate over the census interval.
///
/// Pure and driven from raw `ProcRow`s: the co-varying-fixture trap for this one
/// is a long-lived process with a low cumulative ratio but a large recent burst,
/// which the cumulative lens ranks last and this one ranks first. A process
/// present in only ONE of the two cycles has no rate and is skipped, and a pid
/// whose cumulative time went DOWN was reused by a different process — also
/// skipped, never recorded as a negative or a spurious spike.
pub fn recent_cpu_consumers(
    prev: Option<&CpuSnapshot>,
    rows: &[ProcRow],
    at: DateTime<Utc>,
    config: &CensusConfig,
    limit: usize,
) -> Vec<CpuConsumer> {
    let Some(prev) = prev else {
        return Vec::new();
    };
    let dt = (at - prev.at).num_milliseconds() as f64 / 1000.0;
    if dt <= 0.0 {
        return Vec::new();
    }

    // (proc_count, Σ Δcpu_secs) per label.
    let mut groups: BTreeMap<String, (u32, f64)> = BTreeMap::new();
    for r in rows {
        let Some(&prev_cpu) = prev.cpu_by_pid.get(&r.pid) else {
            continue;
        };
        if r.cpu_secs < prev_cpu {
            // pid reuse: a new process inherited the number; its cumulative time
            // is lower than the one we recorded. Not this process's burn.
            continue;
        }
        let delta = r.cpu_secs - prev_cpu;
        if delta <= 0.0 {
            continue;
        }
        let e = groups.entry(cpu_label(r, config)).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += delta;
    }

    let mut out: Vec<CpuConsumer> = groups
        .into_iter()
        .filter_map(|(name, (proc_count, sum))| {
            let percent_of_one_core = sum * 100.0 / dt;
            (percent_of_one_core > 0.0).then_some(CpuConsumer {
                name,
                proc_count,
                percent_of_one_core,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.percent_of_one_core
            .partial_cmp(&a.percent_of_one_core)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(limit);
    out
}

/// One census cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Census {
    pub id: Uuid,
    #[serde(with = "crate::wire_time")]
    pub taken_at: DateTime<Utc>,
    pub total_procs: u32,
    pub agent_sessions: Vec<AgentSession>,
    pub ide_helpers: HelperRollup,
    pub orphans: OrphanCensus,
    pub tmux_sessions: Vec<TmuxSessionInfo>,
    pub app_groups: Vec<AppGroup>,
    pub monitor_agents: Vec<MonitorAgent>,
    /// Deduplicated total across all monitor patterns. **Use this, not the sum of
    /// `monitor_agents`.**
    pub monitor_total: MonitorTotal,
    /// True when `tmux` could not be reached at all (not installed, or no
    /// server). Distinguished from "zero sessions" so the UI can say "no tmux"
    /// rather than implying a clean machine.
    pub tmux_available: bool,
    /// The recent-rate CPU consumers, differenced against the previous census
    /// (`banshee-aen`). Empty on the first census after a daemon start, and for a
    /// census whose predecessor shared no pids. The CPU/thermal who-line reads
    /// this; every other dimension is attributed by resident size or cumulative
    /// CPU.
    pub cpu_consumers: Vec<CpuConsumer>,
    /// Honest kernel-jetsam kills observed in this census's bounded window —
    /// `rf:high` / `per-process-limit` / `vm-pageshortage`, EXCLUDING the routine
    /// `idle-exit rf:low` daemon housekeeping (`banshee-2dq`). This is what the
    /// Jetsam dimension bands on; it replaces the sample-tier sysctl counter,
    /// which stayed 0 through 170 real kills. `None` when `log show` could not be
    /// run (a non-macOS build) or on a pre-v15 census row — never coerced to 0,
    /// which would invent a calm machine. Windows do not overlap (each starts
    /// where the last ended), so summing across censuses does not double-count.
    #[serde(default)]
    pub pressure_kills: Option<u64>,
}

impl Census {
    pub fn stale_sessions(&self) -> impl Iterator<Item = &AgentSession> {
        self.agent_sessions.iter().filter(|s| s.is_stale)
    }

    /// Sessions safe to reap: stale, and not in a busy tmux pane.
    ///
    /// The dirty-worktree rail lives with the ACTION, not here — it needs a
    /// `git` call per candidate, and a census must not shell out per session.
    pub fn reapable_tmux_sessions(&self) -> impl Iterator<Item = &TmuxSessionInfo> {
        self.tmux_sessions
            .iter()
            .filter(|t| t.is_stale && !t.is_busy)
    }
}

/// Runs one census cycle.
pub struct CensusCollector<R: CommandRunner> {
    runner: R,
    config: CensusConfig,
    /// The previous cycle's per-pid CPU, to difference the recent-rate consumers
    /// against. Interior-mutable because the collector is `Arc`-shared and called
    /// once per census tick, serially — the one piece of state the census keeps
    /// between cycles.
    prev_cpu: Mutex<Option<CpuSnapshot>>,
    /// The timestamp of the previous census, so the jetsam-kill window is exactly
    /// the gap since then (windows tile without overlap or gap) rather than a
    /// fixed `--last 5m` that drifts. `None` on the first cycle after a daemon
    /// start, which falls back to the capped window. Same interior-mutability
    /// rationale as `prev_cpu` (`banshee-2dq`).
    last_census_at: Mutex<Option<DateTime<Utc>>>,
}

impl<R: CommandRunner> CensusCollector<R> {
    pub fn new(runner: R, config: CensusConfig) -> Self {
        Self {
            runner,
            config,
            prev_cpu: Mutex::new(None),
            last_census_at: Mutex::new(None),
        }
    }

    /// Honest jetsam kills in the window since the previous census, capped at
    /// [`JETSAM_WINDOW_CAP_SECS`]. `--last <secs>s` is relative to when `log show`
    /// runs (≈ `at`), so consecutive windows tile: each starts where the last
    /// ended. `None` when `log show` cannot be spawned (a non-macOS build) — the
    /// runner's `Err` is spawn failure, and an unmeasurable window must read as
    /// unknown, not as zero kills.
    fn pressure_kills(&self, at: DateTime<Utc>) -> Option<u64> {
        let window_secs = {
            let mut last = self
                .last_census_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let secs = match *last {
                Some(prev) => (at - prev).num_seconds().clamp(0, JETSAM_WINDOW_CAP_SECS),
                None => JETSAM_WINDOW_CAP_SECS,
            };
            *last = Some(at);
            secs
        };
        let last_arg = format!("{window_secs}s");
        let out = self
            .runner
            .run(
                "/usr/bin/log",
                &["show", "--last", &last_arg, "--predicate", JETSAM_PREDICATE],
            )
            .ok()?;
        Some(count_honest_kills(&out))
    }

    pub fn config(&self) -> &CensusConfig {
        &self.config
    }

    /// Take a census, stamped at `at`.
    ///
    /// The timestamp is injected for the same reason the cheap tier's is: a
    /// collector that reads the clock cannot be placed on a timeline.
    pub fn collect_at(&self, at: DateTime<Utc>) -> Result<Census> {
        let core = self
            .runner
            .run("ps", &["-Ao", "pid=,ppid=,rss=,etime=,time=,tty=,comm="])?;
        let args_out = self.runner.run("ps", &["-Ao", "pid=,args="])?;
        let rows = parse::merge_ps(
            parse::parse_ps_core(&core),
            &parse::parse_ps_args(&args_out),
        );

        let (agent_sessions, ide_helpers) = self.classify_agents(&rows);
        let orphans = self.classify_orphans(&rows);
        let app_groups = self.classify_app_groups(&rows);
        let (monitor_agents, monitor_total) = self.classify_monitor_agents(&rows);
        let (tmux_sessions, tmux_available) = self.classify_tmux(at);

        // The recent-rate CPU consumers, differenced against the previous
        // cycle's per-pid times (`banshee-aen`). One lock: read the predecessor,
        // rank, then replace it with this cycle's snapshot for the next tick.
        let cpu_consumers = {
            let mut prev = self.prev_cpu.lock().unwrap_or_else(|e| e.into_inner());
            let consumers =
                recent_cpu_consumers(prev.as_ref(), &rows, at, &self.config, CPU_CONSUMER_CAP);
            *prev = Some(cpu_snapshot(&rows, at));
            consumers
        };

        // `lsof` runs ONLY for sessions already flagged stale. `perf-scan` calls
        // it per agent pid; at a 5-minute cadence over nine sessions that is
        // wasted I/O, and lsof is the slowest thing in the census.
        let mut agent_sessions = agent_sessions;
        for s in agent_sessions.iter_mut().filter(|s| s.is_stale) {
            s.cwd = self.cwd_of(s.pid);
        }

        // Honest jetsam kills over the window since the last census (`banshee-2dq`).
        // The `log show` call is the fifth and heaviest subprocess in the census
        // tier — affordable at a 5-minute cadence, off-reactor via spawn_blocking,
        // and never on the cheap syscall tier (ADR-0007).
        let pressure_kills = self.pressure_kills(at);

        Ok(Census {
            id: Uuid::new_v4(),
            taken_at: at,
            total_procs: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            agent_sessions,
            ide_helpers,
            orphans,
            tmux_sessions,
            app_groups,
            monitor_agents,
            monitor_total,
            tmux_available,
            cpu_consumers,
            pressure_kills,
        })
    }

    pub fn collect(&self) -> Result<Census> {
        self.collect_at(Utc::now())
    }

    fn classify_agents(&self, rows: &[ProcRow]) -> (Vec<AgentSession>, HelperRollup) {
        let mut sessions = Vec::new();
        let mut helpers = HelperRollup::default();

        for r in rows
            .iter()
            .filter(|r| self.config.is_agent_cli(r.program()))
        {
            if r.is_ide_spawned() {
                helpers.count += 1;
                helpers.rss_bytes = helpers.rss_bytes.saturating_add(r.rss_bytes());
                continue;
            }
            sessions.push(AgentSession {
                pid: r.pid,
                program: r.program().to_string(),
                rss_bytes: r.rss_bytes(),
                age_secs: r.etime_secs,
                age_days: r.age_days(),
                cpu_secs: r.cpu_secs,
                tty: r.tty.clone(),
                is_stale: r.age_days() >= self.config.stale_days,
                cwd: None,
            });
        }
        // Oldest first: the reap candidates belong at the top of any list.
        sessions.sort_by_key(|s| std::cmp::Reverse(s.age_secs));
        (sessions, helpers)
    }

    fn classify_orphans(&self, rows: &[ProcRow]) -> OrphanCensus {
        let mut c = OrphanCensus::default();
        let mut by_program: std::collections::HashMap<String, (u32, u64)> =
            std::collections::HashMap::new();

        for r in rows
            .iter()
            .filter(|r| self.config.is_orphan_candidate(&r.args))
        {
            c.total_count += 1;
            c.total_rss_bytes = c.total_rss_bytes.saturating_add(r.rss_bytes());
            if r.ppid == 1 {
                c.orphan_count += 1;
                c.orphan_rss_bytes = c.orphan_rss_bytes.saturating_add(r.rss_bytes());
                let e = by_program.entry(r.program().to_string()).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.saturating_add(r.rss_bytes());
            }
        }

        c.orphans_by_program = by_program
            .into_iter()
            .map(|(program, (count, rss_bytes))| ProgramCount {
                program,
                count,
                rss_bytes,
            })
            .collect();
        // Descending by count, then name — a stable order so the UI does not
        // reshuffle between cycles for equal counts.
        c.orphans_by_program.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.program.cmp(&b.program))
        });
        c
    }

    fn classify_app_groups(&self, rows: &[ProcRow]) -> Vec<AppGroup> {
        let mut groups: std::collections::HashMap<&str, (u32, u64)> =
            std::collections::HashMap::new();
        for r in rows {
            for name in self.config.app_groups_matching(&r.args) {
                let e = groups.entry(name).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.saturating_add(r.rss_bytes());
            }
        }
        let mut out: Vec<AppGroup> = groups
            .into_iter()
            .map(|(name, (proc_count, rss_bytes))| AppGroup {
                name: name.to_string(),
                proc_count,
                rss_bytes,
            })
            .collect();
        // Biggest footprint first — that is the quit-and-relaunch shortlist.
        out.sort_by(|a, b| {
            b.rss_bytes
                .cmp(&a.rss_bytes)
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    fn classify_monitor_agents(&self, rows: &[ProcRow]) -> (Vec<MonitorAgent>, MonitorTotal) {
        // (proc_count, rss, Σ cpu_secs, max lifetime)
        let mut agents: std::collections::HashMap<&str, (u32, u64, f64, u64)> =
            std::collections::HashMap::new();
        for r in rows {
            for name in self.config.monitor_agents_matching(&r.args) {
                let e = agents.entry(name).or_insert((0, 0, 0.0, 0));
                e.0 += 1;
                e.1 = e.1.saturating_add(r.rss_bytes());
                e.2 += r.cpu_secs;
                e.3 = e.3.max(r.etime_secs);
            }
        }
        // The deduplicated population: each process counted once, however many
        // patterns it matched. Summing the per-agent list over-counts, and a real
        // machine had a pid matching two patterns.
        let mut total = MonitorTotal::default();
        for r in rows.iter().filter(|r| {
            self.config
                .monitor_agents_matching(&r.args)
                .next()
                .is_some()
        }) {
            total.proc_count += 1;
            total.rss_bytes = total.rss_bytes.saturating_add(r.rss_bytes());
            total.cpu_secs_total += r.cpu_secs;
            total.longest_life_secs = total.longest_life_secs.max(r.etime_secs);
        }
        total.percent_of_one_core = if total.longest_life_secs == 0 {
            0.0
        } else {
            total.cpu_secs_total * 100.0 / total.longest_life_secs as f64
        };

        let mut out: Vec<MonitorAgent> = agents
            .into_iter()
            .map(|(name, (proc_count, rss_bytes, cpu, life))| MonitorAgent {
                name: name.to_string(),
                proc_count,
                rss_bytes,
                cpu_secs_total: cpu,
                // Divide by the LONGEST-lived process in the group, matching
                // perf-scan. Guarded: a group whose whole lifetime is 0 would
                // otherwise produce inf and exempt itself from every threshold.
                percent_of_one_core: if life == 0 {
                    0.0
                } else {
                    cpu * 100.0 / life as f64
                },
                longest_life_secs: life,
            })
            .collect();
        out.sort_by(|a, b| {
            b.percent_of_one_core
                .partial_cmp(&a.percent_of_one_core)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.name.cmp(&b.name))
        });
        (out, total)
    }

    fn classify_tmux(&self, at: DateTime<Utc>) -> (Vec<TmuxSessionInfo>, bool) {
        let out = self.runner.run(
            "tmux",
            &[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}",
            ],
        );
        // tmux missing entirely is a fact about the machine, not a census
        // failure. Distinguish it from "zero sessions" so the UI does not imply
        // a clean machine when it simply could not look.
        let Ok(out) = out else {
            return (Vec::new(), false);
        };

        let now = at.timestamp();
        let sessions = parse::parse_tmux_panes(&out)
            .into_iter()
            .map(|p| {
                let idle_days = p.idle_days(now);
                let is_busy = self.config.is_busy_command(&p.command_for_matching());
                TmuxSessionInfo {
                    name: p.session,
                    pane_command: p.current_command,
                    // A session whose age is UNKNOWN is never stale. "We don't
                    // know" and "safe to kill" are different answers.
                    is_stale: idle_days.is_some_and(|d| d >= self.config.stale_days as f64),
                    is_busy,
                    idle_days,
                }
            })
            .collect();
        (sessions, true)
    }

    fn cwd_of(&self, pid: u32) -> Option<String> {
        let pid = pid.to_string();
        let out = self
            .runner
            .run("lsof", &["-a", "-p", &pid, "-d", "cwd", "-Fn"])
            .ok()?;
        parse::parse_lsof_cwd(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::exec::testing::FakeRunner;
    use super::*;

    const PS_CORE: &str = include_str!("../../../../tests/fixtures/census/ps-core.txt");
    const PS_ARGS: &str = include_str!("../../../../tests/fixtures/census/ps-args.txt");
    const TMUX: &str = include_str!("../../../../tests/fixtures/census/tmux-panes.txt");
    const LSOF: &str = include_str!("../../../../tests/fixtures/census/lsof-cwd.txt");
    const LOG_KILLS: &str = include_str!("../../../../tests/fixtures/census/log-show-kills.txt");

    const PS_CORE_CMD: &str = "ps -Ao pid=,ppid=,rss=,etime=,time=,tty=,comm=";
    const PS_ARGS_CMD: &str = "ps -Ao pid=,args=";
    const TMUX_CMD: &str = "tmux list-panes -a -F #{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}";

    fn at() -> DateTime<Utc> {
        // 1788190000 is the activity timestamp in the tmux fixture, so the two
        // "recent" sessions are ~0 days idle and `example-idle` is ~3.4 days.
        DateTime::from_timestamp(1_788_190_000, 0).unwrap()
    }

    fn runner() -> FakeRunner {
        FakeRunner::new()
            .with(PS_CORE_CMD, PS_CORE)
            .with(PS_ARGS_CMD, PS_ARGS)
            .with(TMUX_CMD, TMUX)
            .with("lsof -a -p 88120 -d cwd -Fn", LSOF)
            .with("lsof -a -p 77001 -d cwd -Fn", LSOF)
    }

    fn collector(r: FakeRunner) -> CensusCollector<FakeRunner> {
        CensusCollector::new(r, CensusConfig::default())
    }

    // ---- agent sessions --------------------------------------------------

    /// The load-bearing split, end to end through the collector.
    #[test]
    fn interactive_sessions_are_listed_and_helpers_are_rolled_up() {
        let c = collector(runner()).collect_at(at()).unwrap();

        // Fixture: 5 claude on a tty, 1 claude on ??, 1 kiro-cli on ??.
        assert_eq!(c.agent_sessions.len(), 5, "{:?}", c.agent_sessions);
        assert!(c.agent_sessions.iter().all(|s| s.tty.starts_with("ttys")));
        assert_eq!(c.ide_helpers.count, 2, "one claude + one kiro-cli on ??");
        assert!(c.ide_helpers.rss_bytes > 0);
    }

    /// Oldest first, so reap candidates sort to the top.
    #[test]
    fn sessions_are_ordered_oldest_first() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let ages: Vec<u64> = c.agent_sessions.iter().map(|s| s.age_secs).collect();
        let mut sorted = ages.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(ages, sorted, "must be descending by age");
        assert_eq!(c.agent_sessions[0].pid, 88120, "the 8-day session is first");
    }

    /// Staleness comes from the dashed `etime`, at the configured threshold.
    #[test]
    fn multi_day_sessions_are_flagged_stale() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let stale: Vec<u32> = c.stale_sessions().map(|s| s.pid).collect();
        // 88120 is 8 days, 77001 is 6 days; the rest are hours.
        assert_eq!(stale, vec![88120, 77001], "got {stale:?}");
    }

    #[test]
    fn the_stale_threshold_is_configurable() {
        let cfg = CensusConfig::default().with_overlay(toml::from_str("stale_days = 10").unwrap());
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();
        assert_eq!(c.stale_sessions().count(), 0, "nothing is 10 days old");
    }

    // ---- the lsof rule --------------------------------------------------

    /// `lsof` runs ONLY for flagged sessions. It is the slowest call in the
    /// census, and `perf-scan` pays it per agent pid. Mutation-proof: drop the
    /// `.filter(|s| s.is_stale)` and this sees 5 lsof calls instead of 2.
    #[test]
    fn lsof_runs_only_for_stale_sessions() {
        let r = runner();
        let c = collector(r);
        let census = c.collect_at(at()).unwrap();

        let lsof_calls = c.runner.calls_to("lsof");
        assert_eq!(
            lsof_calls.len(),
            2,
            "expected one lsof per stale session, got {lsof_calls:?}"
        );
        assert!(lsof_calls.iter().any(|k| k.contains("-p 88120")));
        assert!(lsof_calls.iter().any(|k| k.contains("-p 77001")));

        // Non-stale sessions must have no cwd, because nothing looked.
        for s in census.agent_sessions.iter().filter(|s| !s.is_stale) {
            assert!(s.cwd.is_none(), "pid {} should have no cwd", s.pid);
        }
        for s in census.stale_sessions() {
            assert_eq!(s.cwd.as_deref(), Some("/Users/example/Code/banshee"));
        }
    }

    /// The whole census must be two `ps` calls, one `tmux` call and one
    /// `/usr/bin/log show` call, regardless of how many processes exist.
    /// Mutation-proof: move the `ps` call inside a per-process loop and this
    /// explodes.
    #[test]
    fn a_census_costs_a_fixed_number_of_spawns() {
        let r = runner();
        let c = collector(r);
        c.collect_at(at()).unwrap();

        let calls = c.runner.calls();
        assert_eq!(c.runner.calls_to("ps").len(), 2, "{calls:?}");
        assert_eq!(c.runner.calls_to("tmux").len(), 1, "{calls:?}");
        // One log-show probe per census for the honest jetsam count, fixed no
        // matter how many processes or kills there are (`banshee-2dq`).
        assert_eq!(c.runner.calls_to("/usr/bin/log").len(), 1, "{calls:?}");
        // 2 ps + 1 tmux + 2 lsof (stale sessions only) + 1 log = 6, against 20
        // processes.
        assert_eq!(calls.len(), 6, "{calls:?}");
    }

    /// The honest jetsam count is the crux of `banshee-2dq`: the fixture has six
    /// `memorystatus: killing` lines, but only THREE are genuine sustained-pressure
    /// kills (`vm-pageshortage`, `per-process-limit`, `rf:high`). The other three
    /// are noise a naive `contains("killing")` filter would count — two routine
    /// `idle-exit, rf:low` reaps and one kill with no pressure reason at all.
    /// Counting all six is exactly the false-positive generator this replaced.
    /// Mutation-proof: drop the `is_honest_pressure_kill` filter (count every
    /// line) and this reads 6; treat `idle-exit rf:low` as honest and it reads 5.
    #[test]
    fn only_genuine_pressure_kills_are_counted_not_idle_exit_noise() {
        assert_eq!(
            count_honest_kills(LOG_KILLS),
            3,
            "3 honest kills; idle-exit rf:low and reasonless kills are noise"
        );
        // The discriminating lines, one property at a time.
        assert!(is_honest_pressure_kill(
            "memorystatus: killing_top_process pid 1 [x] (vm-pageshortage)"
        ));
        assert!(is_honest_pressure_kill(
            "memorystatus: killing_specific_process pid 2 [y] (per-process-limit)"
        ));
        assert!(is_honest_pressure_kill(
            "memorystatus: killing_top_process pid 3 [z] (rf:high)"
        ));
        assert!(
            !is_honest_pressure_kill(
                "memorystatus: killing_top_process pid 4 [w] (idle-exit, rf:low)"
            ),
            "routine idle-exit reaping is not memory pressure"
        );
        assert!(
            !is_honest_pressure_kill("memorystatus: killing_idle_process pid 5 [q] ()"),
            "a kill with no pressure reason is not counted"
        );
        assert!(
            !is_honest_pressure_kill("some unrelated kernel line"),
            "a line without the kill marker is not a kill"
        );
    }

    /// The honest count reaches the stored census through the collector, from the
    /// real `/usr/bin/log show` output shape. Mutation-proof: stop calling
    /// `pressure_kills` in `collect_at` (leave the field `None`) and this fails.
    #[test]
    fn the_census_records_the_honest_jetsam_count() {
        let first_window = format!("{JETSAM_WINDOW_CAP_SECS}s");
        let log_key =
            format!("/usr/bin/log show --last {first_window} --predicate {JETSAM_PREDICATE}");
        let r = runner().with(&log_key, LOG_KILLS);
        let c = collector(r);
        let census = c.collect_at(at()).unwrap();
        assert_eq!(census.pressure_kills, Some(3));
    }

    // ---- orphans --------------------------------------------------------

    #[test]
    fn orphans_are_counted_separately_from_the_total_helper_population() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let o = &c.orphans;
        // Fixture helpers: 3 mcp-servers (2 at ppid 1), rust-language-server
        // (ppid 1), tree-sitter-cli (ppid 22629) = 5 total, 3 orphaned.
        assert_eq!(o.total_count, 5, "{o:?}");
        assert_eq!(o.orphan_count, 3, "{o:?}");
        assert!(o.orphan_rss_bytes < o.total_rss_bytes);
    }

    #[test]
    fn orphans_are_broken_down_by_program_descending() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let names: Vec<&str> = c
            .orphans
            .orphans_by_program
            .iter()
            .map(|p| p.program.as_str())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"example-notes-mcp-server"));
        assert!(names.contains(&"rust-language-server"));
        // The non-orphan (ppid 22629) must NOT appear.
        assert!(!names.contains(&"tree-sitter-cli"));
    }

    // ---- app groups -----------------------------------------------------

    #[test]
    fn app_groups_aggregate_by_configured_name() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let chrome = c
            .app_groups
            .iter()
            .find(|g| g.name == "Google Chrome")
            .expect("Chrome group");
        assert_eq!(chrome.proc_count, 2);
        assert_eq!(chrome.rss_bytes, (3_244_032 + 1_048_576) * 1024);
    }

    #[test]
    fn app_groups_are_ordered_by_footprint() {
        let cfg = CensusConfig::default().with_overlay(
            toml::from_str(r#"app_groups = ["Google Chrome", "example-log-shipper"]"#).unwrap(),
        );
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();
        assert_eq!(c.app_groups[0].name, "Google Chrome", "biggest first");
    }

    // ---- monitoring agents ----------------------------------------------

    /// Cumulative CPU over lifetime, not `%CPU`.
    #[test]
    fn monitor_agents_report_cumulative_cpu_over_the_longest_lifetime() {
        let cfg = CensusConfig::default().with_overlay(
            toml::from_str(r#"monitor_agents = ["endpointagent", "scan-helper", "log-shipper"]"#)
                .unwrap(),
        );
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();
        assert_eq!(c.monitor_agents.len(), 3, "{:?}", c.monitor_agents);

        let ep = c
            .monitor_agents
            .iter()
            .find(|a| a.name == "endpointagent")
            .unwrap();
        // 176:45.22 of CPU over 21:40:00 of life.
        let expected = (176.0 * 60.0 + 45.22) * 100.0 / (21.0 * 3600.0 + 40.0 * 60.0);
        assert!(
            (ep.percent_of_one_core - expected).abs() < 1e-6,
            "got {}",
            ep.percent_of_one_core
        );
        assert!(ep.percent_of_one_core > 13.0 && ep.percent_of_one_core < 14.0);
    }

    /// A MULTI-process agent group divides by the LONGEST lifetime, not the sum.
    ///
    /// This test exists because the single-process version above was a FALSE PIN:
    /// with one process per group, `max(lifetime)` and `sum(lifetime)` are
    /// identical, so swapping them changed nothing and the mutation passed. The
    /// fixture now gives `scan-helper` two processes with different lifetimes.
    ///
    /// Dividing by the sum would understate a multi-process agent's cost roughly
    /// in proportion to how many processes it runs — exactly the agents most
    /// worth watching.
    #[test]
    fn a_multi_process_agent_divides_by_the_longest_lifetime_not_the_sum() {
        let cfg = CensusConfig::default()
            .with_overlay(toml::from_str(r#"monitor_agents = ["scan-helper"]"#).unwrap());
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();

        let insp = c
            .monitor_agents
            .iter()
            .find(|a| a.name == "scan-helper")
            .unwrap();
        assert_eq!(insp.proc_count, 2, "the fixture must have two scan-helpers");

        let cpu: f64 = (206.0 * 60.0 + 53.07) + (30.0 * 60.0);
        let longest: f64 = 21.0 * 3600.0 + 40.0 * 60.0; // the 21:40:00 process
        let sum_of_lives = longest + 2.0 * 3600.0; // + the 02:00:00 one

        let by_max = cpu * 100.0 / longest;
        let by_sum = cpu * 100.0 / sum_of_lives;
        assert!(
            (by_max - by_sum).abs() > 1.0,
            "fixture must make the two formulas differ meaningfully"
        );

        assert!(
            (insp.percent_of_one_core - by_max).abs() < 1e-6,
            "expected {by_max} (÷ longest life), got {} — dividing by the sum \
             would give {by_sum}",
            insp.percent_of_one_core
        );
    }

    #[test]
    fn monitor_agents_are_ordered_by_cost() {
        let cfg = CensusConfig::default().with_overlay(
            toml::from_str(r#"monitor_agents = ["endpointagent", "scan-helper", "log-shipper"]"#)
                .unwrap(),
        );
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();
        let pcts: Vec<f64> = c
            .monitor_agents
            .iter()
            .map(|a| a.percent_of_one_core)
            .collect();
        let mut sorted = pcts.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        assert_eq!(pcts, sorted, "most expensive first");
        // The DLP scan helper (206:53.07) outranks the endpoint agent (176:45.22).
        assert_eq!(c.monitor_agents[0].name, "scan-helper");
    }

    /// Overlapping patterns double-count in the per-agent list; the total must
    /// not. (The per-agent list also can't be summed for a second, independent
    /// reason — heterogeneous denominators — documented on `MonitorAgent`.)
    ///
    /// Mutation-proof, verified: counting once per MATCH rather than once per
    /// PROCESS makes this fail with 2 against an expected 1. Note that the first
    /// attempt at that mutation silently failed to apply after `cargo fmt`
    /// reshaped the target code, which is indistinguishable from a caught
    /// mutation; the harness now refuses to report a run whose target text is absent.
    #[test]
    fn the_monitor_total_counts_each_process_once_despite_overlapping_patterns() {
        // Both patterns match pid 55001's command line
        // (/Library/SystemExtensions/.../com.example.endpointagent...).
        let cfg = CensusConfig::default().with_overlay(
            toml::from_str(r#"monitor_agents = ["endpointagent", "SystemExtensions"]"#).unwrap(),
        );
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();

        assert_eq!(c.monitor_agents.len(), 2, "both patterns matched");
        let summed: f64 = c.monitor_agents.iter().map(|a| a.percent_of_one_core).sum();
        let summed_procs: u32 = c.monitor_agents.iter().map(|a| a.proc_count).sum();

        assert_eq!(summed_procs, 2, "the per-agent list counts the pid twice");
        assert_eq!(
            c.monitor_total.proc_count, 1,
            "the total must count it once"
        );
        assert!(
            c.monitor_total.percent_of_one_core < summed - 1.0,
            "total {} should be well below the naive sum {summed}",
            c.monitor_total.percent_of_one_core
        );
    }

    /// The lifetime denominator travels with the figure, so a consumer can refuse
    /// to band on a ratio derived from a process that started a minute ago.
    /// Measured: a log shipper 109s old reported 14.6% off 16s of CPU.
    #[test]
    fn the_lifetime_denominator_is_exposed_alongside_the_percentage() {
        let cfg = CensusConfig::default()
            .with_overlay(toml::from_str(r#"monitor_agents = ["log-shipper"]"#).unwrap());
        let c = CensusCollector::new(runner(), cfg)
            .collect_at(at())
            .unwrap();
        let a = &c.monitor_agents[0];
        assert_eq!(a.longest_life_secs, 21 * 3600 + 40 * 60);
        // The percentage is exactly cpu ÷ that lifetime.
        let expected = a.cpu_secs_total * 100.0 / a.longest_life_secs as f64;
        assert!((a.percent_of_one_core - expected).abs() < 1e-9);
        assert_eq!(c.monitor_total.longest_life_secs, a.longest_life_secs);
    }

    /// The generic committed default must find nothing interesting on the
    /// fixture — proof that the real names genuinely come from the overlay.
    #[test]
    fn the_default_monitor_list_finds_nothing_in_the_fixture() {
        let c = collector(runner()).collect_at(at()).unwrap();
        assert!(
            c.monitor_agents.is_empty(),
            "committed defaults must not match synthetic agents: {:?}",
            c.monitor_agents
        );
    }

    // ---- tmux -----------------------------------------------------------

    #[test]
    fn tmux_sessions_are_classified_by_idle_age_and_busyness() {
        let c = collector(runner()).collect_at(at()).unwrap();
        assert!(c.tmux_available);
        assert_eq!(c.tmux_sessions.len(), 5);

        let by = |n: &str| {
            c.tmux_sessions
                .iter()
                .find(|t| t.name == n)
                .unwrap()
                .clone()
        };

        // Fresh, idle shell.
        let p1 = by("banshee-p1-worker");
        assert!(!p1.is_stale);
        assert!(!p1.is_busy);

        // Capitalized Python must count as busy.
        let p2 = by("banshee-p2-worker");
        assert!(p2.is_busy, "Python must match python*");

        // cargo is busy.
        assert!(by("example-build").is_busy);

        // ~3.4 days idle, zsh: the one real reap candidate.
        let idle = by("example-idle");
        assert!(idle.is_stale, "idle_days = {:?}", idle.idle_days);
        assert!(!idle.is_busy);
    }

    /// Only stale AND not-busy sessions are reapable. Mutation-proof: drop
    /// `!t.is_busy` and a mid-build pane becomes a candidate — which is how you
    /// destroy someone's work.
    #[test]
    fn a_busy_pane_is_never_reapable_however_old() {
        let r = runner().with(
            TMUX_CMD,
            // An ancient pane running cargo.
            "old-build|999|cargo|1787000000\nold-idle|998|zsh|1787000000\n",
        );
        let c = collector(r).collect_at(at()).unwrap();
        let reapable: Vec<&str> = c
            .reapable_tmux_sessions()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(reapable, vec!["old-idle"], "the build must be spared");
    }

    /// A session with no activity timestamp must never be reapable.
    #[test]
    fn a_session_with_unknown_age_is_never_reapable() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let undated = c
            .tmux_sessions
            .iter()
            .find(|t| t.name == "example-undated")
            .unwrap();
        assert_eq!(undated.idle_days, None);
        assert!(!undated.is_stale, "unknown age must not read as stale");
        assert!(
            !c.reapable_tmux_sessions()
                .any(|t| t.name == "example-undated"),
            "must not be a reap candidate"
        );
    }

    /// "tmux is not installed" and "tmux has no sessions" are different answers,
    /// and conflating them tells the user the machine is clean when we could not
    /// look. Mutation-proof: return `true` in the `Err` arm and this fails.
    #[test]
    fn a_missing_tmux_is_reported_as_unavailable_not_as_zero_sessions() {
        let r = runner().unavailable("tmux");
        let c = collector(r).collect_at(at()).unwrap();
        assert!(!c.tmux_available, "must not claim tmux was readable");
        assert!(c.tmux_sessions.is_empty());
        // The rest of the census still works.
        assert!(!c.agent_sessions.is_empty());
        assert_eq!(c.total_procs, 21);
    }

    /// tmux running with no sessions is genuinely zero, and available.
    #[test]
    fn tmux_with_no_sessions_is_available_and_empty() {
        let r = runner().with(TMUX_CMD, "");
        let c = collector(r).collect_at(at()).unwrap();
        assert!(c.tmux_available);
        assert!(c.tmux_sessions.is_empty());
    }

    // ---- whole-census properties ----------------------------------------

    #[test]
    fn total_procs_counts_every_parsed_row() {
        let c = collector(runner()).collect_at(at()).unwrap();
        assert_eq!(c.total_procs, 21, "the fixture has 21 rows");
    }

    #[test]
    fn the_injected_timestamp_is_used_verbatim() {
        let c = collector(runner()).collect_at(at()).unwrap();
        assert_eq!(c.taken_at, at());
    }

    /// A failing `ps` is fatal to the census — unlike tmux, there is no census
    /// without it, and returning an empty one would read as "nothing running".
    #[test]
    fn a_failing_ps_is_an_error_not_an_empty_census() {
        let r = runner().unavailable("ps");
        assert!(collector(r).collect_at(at()).is_err());
    }

    /// The wire contract: camelCase at every depth, including the nested
    /// per-session and per-group objects (Five Rules, rule 3).
    #[test]
    fn serializes_with_camel_case_keys_at_every_depth() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let v: serde_json::Value = serde_json::to_value(&c).unwrap();

        for key in [
            "id",
            "takenAt",
            "totalProcs",
            "agentSessions",
            "ideHelpers",
            "orphans",
            "tmuxSessions",
            "appGroups",
            "monitorAgents",
            "tmuxAvailable",
        ] {
            assert!(v.get(key).is_some(), "missing top-level key {key}");
        }
        assert!(v.get("total_procs").is_none(), "snake_case leaked");

        let s = &v["agentSessions"][0];
        for key in [
            "pid", "rssBytes", "ageSecs", "ageDays", "cpuSecs", "isStale", "cwd",
        ] {
            assert!(s.get(key).is_some(), "missing session key {key}");
        }
        assert!(v["orphans"].get("orphanCount").is_some());
        assert!(
            v["orphans"]["orphansByProgram"][0]
                .get("rssBytes")
                .is_some()
        );
        assert!(v["ideHelpers"].get("rssBytes").is_some());

        assert_eq!(v["takenAt"], "2026-08-31T15:26:40.000000Z");
    }

    /// Nullable fields are present-as-null, never absent — this binds the
    /// encoder. A non-stale session's `cwd` must be an explicit null.
    #[test]
    fn an_absent_cwd_serializes_as_explicit_null() {
        let c = collector(runner()).collect_at(at()).unwrap();
        let v: serde_json::Value = serde_json::to_value(&c).unwrap();
        let fresh = v["agentSessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["isStale"] == false)
            .expect("a non-stale session");
        assert!(fresh.get("cwd").is_some(), "key must be present");
        assert!(fresh["cwd"].is_null(), "and explicitly null");
    }

    // ---- against the real machine ---------------------------------------

    /// A real census must run and produce plausible structure. Invariants only,
    /// so it holds on any Mac in any load condition.
    #[test]
    fn a_real_census_runs_and_is_plausible() {
        let c = CensusCollector::new(super::exec::RealRunner, CensusConfig::default())
            .collect()
            .unwrap();
        assert!(c.total_procs > 50, "got {} processes", c.total_procs);
        // Every listed session has a terminal; every helper does not.
        for s in &c.agent_sessions {
            assert!(
                !s.tty.is_empty() && s.tty != "??",
                "pid {} tty {:?}",
                s.pid,
                s.tty
            );
            assert!(c.config_program_is_known(&s.program));
        }
        // cwd is populated only where we looked.
        for s in c.agent_sessions.iter().filter(|s| !s.is_stale) {
            assert!(s.cwd.is_none());
        }
        assert!(c.orphans.orphan_count <= c.orphans.total_count);
    }

    // ---- recent-rate CPU consumers (banshee-aen) ------------------------

    fn cpu_row(pid: u32, cpu_secs: f64, comm: &str, args: &str) -> ProcRow {
        ProcRow {
            pid,
            ppid: 1,
            rss_kb: 1000,
            etime_secs: 100_000,
            cpu_secs,
            tty: "??".into(),
            comm: comm.into(),
            args: args.into(),
        }
    }

    const CHROME_ARGS: &str =
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome --type=renderer";

    /// The recent-rate lens ranks by CPU burned SINCE the last census, not by
    /// lifetime total, so a long-lived process that has gone quiet loses to a
    /// process that just started working. THE co-varying trap for this feature:
    /// `steady-worker` has ~24× the cumulative CPU of the two Chrome processes
    /// combined (5008 s vs 210 s) yet Chrome ranks FIRST because it burned 180 s
    /// in the last minute against steady-worker's 8. A cumulative lens ranks them
    /// the other way round.
    /// Also proves the two group-defining rules: pids sharing a label (both
    /// Chrome renderers) collapse to one entry with `proc_count == 2` and their
    /// deltas summed, and a process is labelled by its app GROUP when it matches
    /// one, by its `comm` basename otherwise.
    #[test]
    fn recent_cpu_consumers_rank_by_recent_burn_not_lifetime_total() {
        let cfg = CensusConfig::default();
        let t0 = DateTime::from_timestamp(1_788_190_000, 0).unwrap();
        let t1 = t0 + chrono::Duration::seconds(60);

        let prev_rows = [
            cpu_row(100, 10.0, "Google Chrome", CHROME_ARGS),
            cpu_row(101, 20.0, "Google Chrome", CHROME_ARGS),
            cpu_row(200, 5000.0, "steady-worker", "steady-worker"),
            cpu_row(500, 100.0, "idle-daemon", "idle-daemon"),
            cpu_row(400, 900.0, "before-reuse", "before-reuse"),
        ];
        let prev = cpu_snapshot(&prev_rows, t0);

        let now_rows = [
            cpu_row(100, 130.0, "Google Chrome", CHROME_ARGS), // +120
            cpu_row(101, 80.0, "Google Chrome", CHROME_ARGS),  // +60  → Chrome sum 180
            cpu_row(200, 5008.0, "steady-worker", "steady-worker"), // +8
            cpu_row(500, 100.0, "idle-daemon", "idle-daemon"), // +0 → skipped
            cpu_row(400, 2.0, "after-reuse", "after-reuse"), // cumulative DOWN → pid reuse, skipped
            cpu_row(300, 55.0, "newcomer", "newcomer"),      // absent from prev → no rate, skipped
        ];

        let out = recent_cpu_consumers(Some(&prev), &now_rows, t1, &cfg, 12);

        assert_eq!(
            out.len(),
            2,
            "only the two with a real positive delta: {out:?}"
        );
        assert_eq!(out[0].name, "Google Chrome", "biggest recent burn first");
        assert_eq!(out[0].proc_count, 2, "two renderers collapse to one label");
        assert!(
            (out[0].percent_of_one_core - 300.0).abs() < 1e-6,
            "180 s over 60 s = 300% of one core, got {}",
            out[0].percent_of_one_core
        );
        assert_eq!(out[1].name, "steady-worker");
        assert_eq!(out[1].proc_count, 1);
        // The trap, made explicit: steady-worker dwarfs Chrome on lifetime CPU and
        // still ranks below it on the recent lens.
        assert!(out[1].percent_of_one_core < out[0].percent_of_one_core);
        // The skips never appear as consumers.
        for skipped in ["idle-daemon", "after-reuse", "before-reuse", "newcomer"] {
            assert!(
                !out.iter().any(|c| c.name == skipped),
                "{skipped} must not be a consumer: {out:?}"
            );
        }
    }

    /// No previous snapshot (the FIRST census after a start) and a non-positive
    /// interval both yield NO consumers — never a spurious rate. This is what
    /// makes the who-line fall back to the cumulative lens rather than name
    /// nobody, and it is the case that fired minutes after every daemon restart.
    #[test]
    fn recent_cpu_consumers_are_empty_without_a_usable_prior_interval() {
        let cfg = CensusConfig::default();
        let t0 = DateTime::from_timestamp(1_788_190_000, 0).unwrap();
        let rows = [cpu_row(100, 130.0, "Google Chrome", CHROME_ARGS)];

        assert!(
            recent_cpu_consumers(None, &rows, t0, &cfg, 12).is_empty(),
            "no prior snapshot → no rate"
        );
        let prev = cpu_snapshot(&rows, t0);
        assert!(
            recent_cpu_consumers(Some(&prev), &rows, t0, &cfg, 12).is_empty(),
            "dt = 0 → no rate"
        );
    }

    /// The cap is load-bearing: more distinct hot labels than the limit truncate
    /// to the top `limit`, ranked. Mutation-proof: drop the `truncate` and the
    /// full set comes back.
    #[test]
    fn recent_cpu_consumers_truncate_to_the_limit() {
        let cfg = CensusConfig::default();
        let t0 = DateTime::from_timestamp(1_788_190_000, 0).unwrap();
        let t1 = t0 + chrono::Duration::seconds(60);
        let prev_rows: Vec<ProcRow> = (0..6)
            .map(|i| cpu_row(1000 + i, 0.0, &format!("hog{i}"), &format!("hog{i}")))
            .collect();
        let prev = cpu_snapshot(&prev_rows, t0);
        // Each burns i+1 seconds, so hog5 is hottest and hog0 coolest.
        let now_rows: Vec<ProcRow> = (0..6)
            .map(|i| {
                cpu_row(
                    1000 + i,
                    (i + 1) as f64,
                    &format!("hog{i}"),
                    &format!("hog{i}"),
                )
            })
            .collect();

        let out = recent_cpu_consumers(Some(&prev), &now_rows, t1, &cfg, 3);
        assert_eq!(out.len(), 3, "truncated to the limit");
        assert_eq!(
            out.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["hog5", "hog4", "hog3"],
            "the three hottest, ranked"
        );
    }

    impl Census {
        fn config_program_is_known(&self, program: &str) -> bool {
            CensusConfig::default().is_agent_cli(program)
        }
    }

    // ---- the shared census fixture (raw bytes, not our own encoder) ----------
    //
    // The SAME bytes the Swift suite decodes (`WireFormatTests`). Census is ~40
    // fields across seven nested types and the e2e harness pins it only
    // cross-SURFACE; this fixture is the cross-language pin (P6b, `banshee-bej`).

    const FIXTURE_CENSUS: &str = include_str!("../../../../tests/fixtures/census-full.json");

    #[test]
    fn decodes_the_census_fixture_at_full_depth() {
        let c: Census = serde_json::from_str(FIXTURE_CENSUS).unwrap();

        assert_eq!(c.total_procs, 812);
        assert!(c.tmux_available);

        // Sessions: the stale one carries a cwd (lsof ran for it), the live one
        // an EXPLICIT null. Both states must decode distinguishably.
        assert_eq!(c.agent_sessions.len(), 2);
        let stale = &c.agent_sessions[0];
        assert!(stale.is_stale);
        assert_eq!(
            stale.cwd.as_deref(),
            Some("/Users/example/Code/old-project")
        );
        assert_eq!(stale.age_days, 5);
        let live = &c.agent_sessions[1];
        assert!(!live.is_stale);
        assert_eq!(live.cwd, None);

        assert_eq!(c.ide_helpers.count, 48);
        assert_eq!(c.orphans.orphan_count, 23);
        assert_eq!(c.orphans.orphans_by_program.len(), 2);
        assert_eq!(c.orphans.orphans_by_program[0].program, "mcp-server-fetch");

        // tmux: idleDays null is "unknown", not "idle forever"; the busy pane
        // must never look reapable.
        let busy = &c.tmux_sessions[0];
        assert!(busy.is_busy && busy.idle_days.is_none() && !busy.is_stale);
        let stale_tmux = &c.tmux_sessions[1];
        assert_eq!(stale_tmux.idle_days, Some(11.25));
        assert!(stale_tmux.is_stale);
        // The third session is stale AND busy — the case that distinguishes
        // "stale and not busy" from a filter on staleness alone (the recurring
        // co-varying-fixture shape). Only "scratch" is reapable.
        let names: Vec<&str> = c
            .reapable_tmux_sessions()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, ["scratch"]);

        assert_eq!(c.app_groups[0].name, "Chrome");
        assert_eq!(c.app_groups[0].proc_count, 128);

        // The never-sum rule, made distinguishable by construction: the fixture's
        // per-agent percentages sum to 46.0 while the deduplicated global-
        // denominator total is 29.183. A consumer summing the list gets a number
        // this test proves is NOT the honest one.
        let summed: f64 = c.monitor_agents.iter().map(|m| m.percent_of_one_core).sum();
        assert!((summed - 46.0).abs() < 1e-9);
        assert!((c.monitor_total.percent_of_one_core - 29.183).abs() < 1e-9);
        assert!(summed > c.monitor_total.percent_of_one_core);
        assert_eq!(c.monitor_total.proc_count, 7);
        assert_eq!(c.monitor_agents[1].longest_life_secs, 109);

        // The recent-rate CPU consumers (`banshee-aen`): ranked, and NOT limited
        // to stored agent sessions — the top consumer is a browser helper the
        // census does not otherwise size for CPU. This is the coverage the
        // cumulative lens lacks.
        assert_eq!(c.cpu_consumers.len(), 3);
        assert_eq!(c.cpu_consumers[0].name, "Chrome Helper (Renderer)");
        assert_eq!(c.cpu_consumers[0].proc_count, 4);
        assert!((c.cpu_consumers[0].percent_of_one_core - 312.4).abs() < 1e-6);
    }
}
