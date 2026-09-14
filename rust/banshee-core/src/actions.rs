// actions — the two reaps, with `perf-scan`'s safety rails ported as TESTS,
// not vibes (docs/signal-collection.md, "Actions and their safety rails").
//
// Everything here is built on one invariant: **preview and execute share one
// candidate-selection function per action**, so the dry-run a person approved
// and the kill that follows cannot disagree about who is on the list. The
// second invariant follows from the first: **nothing in this module accepts a
// target list from a caller.** PIDs recycle; a list computed earlier — by a
// preview, by a census, by an agent's memory of either — is a list of the
// machine as it WAS, and killing a recycled PID is the disaster case
// (docs/signal-collection.md: "A recycled PID must never be killed").
//
// The rails, in the order `perf-scan` applies them:
//
//   reap_stale_sessions — for each stale tmux session:
//     1. skip if `session_activity` is unset ("we don't know" ≠ "safe to kill");
//     2. skip unless idle days ≥ the configured threshold;
//     3. skip if any pane command matches BUSY_COMMANDS, lowercased first —
//        tmux reports `Python`, not `python3`, and a raw comparison would kill
//        a running build;
//     4. skip if any pane's cwd is a git repo with a DIRTY worktree — "an
//        unstaged fix is not recoverable from tmux scrollback". This rail
//        lives here and not in the census because it costs a `git` call per
//        candidate, and a census must not shell out per session
//        (`Census::reapable_tmux_sessions`' comment is the other half of this
//        sentence).
//
//   reap_orphans — SIGTERM the recomputed list → wait → RECOMPUTE AGAIN →
//     SIGKILL only the pids that are still in the orphan population under the
//     same program name. The second recompute is what makes a recycled PID
//     unkillable: a new process wearing an old pid would have to match the
//     orphan patterns, have ppid 1, AND report the same executable to be
//     touched.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::census::config::CensusConfig;
use crate::census::exec::CommandRunner;
use crate::census::parse;

/// How long a SIGTERM gets before survivors are SIGKILLed — `perf-scan`'s 8s.
pub const DEFAULT_TERM_WAIT: Duration = Duration::from_secs(8);

/// The census's four fields plus `pane_current_path`, which only the
/// dirty-worktree rail needs. A separate format string from the census's on
/// purpose: the census must not pay for a field only actions read.
const TMUX_ACTION_FORMAT: &str =
    "#{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}|#{pane_current_path}";

// --- Wire types --------------------------------------------------------------
//
// camelCase at every depth, nullable fields present-as-null (docs/wire-format.md).
// Deliberately NO timestamp: the report describes the call that produced it, the
// caller knows when it called, and a timestamp would make an otherwise
// deterministic payload impossible to compare byte-for-byte across surfaces —
// which is exactly how the e2e harness pins these routes.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionVerdict {
    /// Execute would kill this session.
    Reap,
    /// A rail matched; execute will not touch it. `reason` says which.
    Spare,
}

/// One stale-or-undatable tmux session, with the verdict and why.
///
/// Sessions younger than the staleness threshold are not candidates and do not
/// appear at all; sessions whose age is UNKNOWN do appear, as `spare`, because
/// "we could not date it" is information the person approving a reap needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCandidate {
    pub session: String,
    /// The pane command as tmux reported it (capitalization intact). When a
    /// busy pane spared the session, this is THAT pane's command.
    pub pane_command: String,
    /// Days idle; `null` when tmux had no activity timestamp — which is not
    /// the same as "idle forever" and never makes a session reapable.
    pub idle_days: Option<f64>,
    /// The pane's working directory; the DIRTY one when that rail fired.
    pub cwd: Option<String>,
    pub verdict: SessionVerdict,
    /// Human wording for the verdict, composed here so every surface says the
    /// same thing (the same rule as `Pressure`'s labels, ADR-0005).
    pub reason: String,
    /// `null` on a preview. On execute: `"killed"`, `"survived"` (the kill was
    /// issued and the session is still there), or `"failed: <detail>"`.
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReapReport {
    /// `"reapStaleSessions"` — matches the `Finding.action` vocabulary.
    pub action: String,
    pub executed: bool,
    /// `false` means tmux could not be reached AT ALL — "could not look", never
    /// to be rendered as "nothing to reap" (the census's own rule).
    pub tmux_available: bool,
    /// The threshold the selection used, so a UI can say "idle ≥ N days".
    pub stale_days: u64,
    pub candidates: Vec<SessionCandidate>,
}

/// One orphaned helper process (matches an orphan pattern, ppid 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanCandidate {
    pub pid: u32,
    pub program: String,
    /// Full command line, so a person can tell WHICH mcp-server this was.
    pub args: String,
    pub rss_bytes: u64,
    pub age_secs: u64,
    /// `null` on a preview. On execute: `"terminated"` (died on SIGTERM) or
    /// `"killed"` (survived the grace period and was SIGKILLed).
    pub outcome: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanReapReport {
    /// `"reapOrphans"` — matches the `Finding.action` vocabulary.
    pub action: String,
    pub executed: bool,
    /// Seconds SIGTERM gets before survivors are SIGKILLed.
    pub term_wait_secs: u64,
    pub candidates: Vec<OrphanCandidate>,
}

// --- The runner ---------------------------------------------------------------

/// Runs the two reap actions. Holds a `dyn CommandRunner` rather than being
/// generic so `AppState` can carry one without infecting every signature; a
/// user-triggered action has no cadence to justify monomorphized dispatch.
pub struct ActionRunner {
    runner: Box<dyn CommandRunner>,
    config: CensusConfig,
    term_wait: Duration,
}

impl ActionRunner {
    pub fn new(runner: Box<dyn CommandRunner>, config: CensusConfig) -> Self {
        Self {
            runner,
            config,
            term_wait: DEFAULT_TERM_WAIT,
        }
    }

    /// Tests use `Duration::ZERO`; production has no reason to touch this.
    pub fn with_term_wait(mut self, wait: Duration) -> Self {
        self.term_wait = wait;
        self
    }

    // ---- reap_stale_sessions ------------------------------------------------

    pub fn preview_stale_sessions(&self) -> Result<SessionReapReport> {
        self.preview_stale_sessions_at(chrono::Utc::now().timestamp())
    }

    pub fn execute_stale_sessions(&self) -> Result<SessionReapReport> {
        self.execute_stale_sessions_at(chrono::Utc::now().timestamp())
    }

    /// The clock is injected for the same reason the census's is: a selector
    /// that reads the clock cannot be tested against a fixed fixture.
    pub fn preview_stale_sessions_at(&self, now_secs: i64) -> Result<SessionReapReport> {
        let (candidates, tmux_available) = self.session_candidates(now_secs)?;
        Ok(SessionReapReport {
            action: "reapStaleSessions".into(),
            executed: false,
            tmux_available,
            stale_days: self.config.stale_days,
            candidates,
        })
    }

    pub fn execute_stale_sessions_at(&self, now_secs: i64) -> Result<SessionReapReport> {
        // THE SAME selection preview ran — recomputed now, trusted from nowhere.
        let (mut candidates, tmux_available) = self.session_candidates(now_secs)?;

        for c in candidates
            .iter_mut()
            .filter(|c| c.verdict == SessionVerdict::Reap)
        {
            // `=` pins an EXACT session name. Without it tmux does prefix
            // matching, and killing `work` would take `work-important` with it.
            let target = format!("={}", c.session);
            if let Err(e) = self.runner.run("tmux", &["kill-session", "-t", &target]) {
                c.outcome = Some(format!("failed: {e}"));
            }
        }

        // Re-list rather than trusting our own kill: `kill-session` cannot
        // report failure through stdout, and "killed" is a claim about the
        // machine, not about a subprocess having been spawned.
        let survivors: std::collections::HashSet<String> = self
            .runner
            .run("tmux", &["list-panes", "-a", "-F", TMUX_ACTION_FORMAT])
            .map(|out| {
                parse::parse_tmux_panes(&out)
                    .into_iter()
                    .map(|p| p.session)
                    .collect()
            })
            // tmux unreachable after the kills usually means the server exited
            // with its last session — nothing is left to survive.
            .unwrap_or_default();

        for c in candidates
            .iter_mut()
            .filter(|c| c.verdict == SessionVerdict::Reap && c.outcome.is_none())
        {
            c.outcome = Some(if survivors.contains(&c.session) {
                "survived".into()
            } else {
                "killed".into()
            });
        }

        Ok(SessionReapReport {
            action: "reapStaleSessions".into(),
            executed: true,
            tmux_available,
            stale_days: self.config.stale_days,
            candidates,
        })
    }

    /// The one selection function. Applies rails 1–4 in `perf-scan`'s order and
    /// returns every stale-or-undatable session with its verdict.
    fn session_candidates(&self, now_secs: i64) -> Result<(Vec<SessionCandidate>, bool)> {
        let out = self
            .runner
            .run("tmux", &["list-panes", "-a", "-F", TMUX_ACTION_FORMAT]);
        // tmux missing is a fact about the machine, not an action failure —
        // but it must be REPORTED as "could not look" (the census's rule).
        let Ok(out) = out else {
            return Ok((Vec::new(), false));
        };

        // Group panes by session, preserving first-seen order. Every rail is
        // evaluated across ALL of a session's panes: one busy pane spares the
        // whole session, because `kill-session` takes the whole session.
        let mut sessions: Vec<(String, Vec<parse::TmuxPane>)> = Vec::new();
        for pane in parse::parse_tmux_panes(&out) {
            match sessions.iter_mut().find(|(name, _)| *name == pane.session) {
                Some((_, panes)) => panes.push(pane),
                None => sessions.push((pane.session.clone(), vec![pane])),
            }
        }

        let mut candidates = Vec::new();
        for (name, panes) in sessions {
            let idle_days = panes[0].idle_days(now_secs);

            // Rail 1: a session we cannot date is never reaped — surfaced as a
            // candidate so the person sees WHY it was passed over.
            let Some(days) = idle_days else {
                candidates.push(SessionCandidate {
                    session: name,
                    pane_command: panes[0].current_command.clone(),
                    idle_days: None,
                    cwd: panes[0].path.clone(),
                    verdict: SessionVerdict::Spare,
                    reason: "idle time unknown — a session that cannot be dated is never reaped"
                        .into(),
                    outcome: None,
                });
                continue;
            };

            // Rail 2: younger than the threshold is not a candidate at all.
            if days < self.config.stale_days as f64 {
                continue;
            }

            // Rail 3: any busy pane spares the session. Matching is on the
            // LOWERCASED command (tmux reports `Python`); the report carries
            // the raw one.
            if let Some(busy) = panes
                .iter()
                .find(|p| self.config.is_busy_command(&p.command_for_matching()))
            {
                candidates.push(SessionCandidate {
                    session: name,
                    pane_command: busy.current_command.clone(),
                    idle_days: Some(days),
                    cwd: busy.path.clone(),
                    verdict: SessionVerdict::Spare,
                    reason: format!(
                        "busy pane ({}) — never reap a session mid-build",
                        busy.current_command
                    ),
                    outcome: None,
                });
                continue;
            }

            // Rail 4: a dirty worktree in ANY pane's cwd spares the session.
            if let Some(candidate) = self.worktree_rail(&panes) {
                candidates.push(SessionCandidate {
                    session: name,
                    idle_days: Some(days),
                    ..candidate
                });
                continue;
            }

            candidates.push(SessionCandidate {
                session: name,
                pane_command: panes[0].current_command.clone(),
                idle_days: Some(days),
                cwd: panes[0].path.clone(),
                verdict: SessionVerdict::Reap,
                reason: format!("idle {days:.1} days, no busy pane, worktree clean"),
                outcome: None,
            });
        }
        Ok((candidates, true))
    }

    /// Rail 4 for one session's panes. `Some` means a pane's worktree spares
    /// the session; the candidate carries the offending pane's details and the
    /// reason (`session` and `idle_days` are filled in by the caller).
    fn worktree_rail(&self, panes: &[parse::TmuxPane]) -> Option<SessionCandidate> {
        let mut checked = std::collections::HashSet::new();
        for pane in panes {
            let spare = |cwd: Option<String>, reason: String| {
                Some(SessionCandidate {
                    session: String::new(),
                    pane_command: pane.current_command.clone(),
                    idle_days: None,
                    cwd,
                    verdict: SessionVerdict::Spare,
                    reason,
                    outcome: None,
                })
            };

            let Some(path) = pane.path.as_deref() else {
                // No cwd means we cannot check for unsaved work, and "we don't
                // know" is never "safe to kill".
                return spare(
                    None,
                    "pane working directory unknown — cannot check for uncommitted work".into(),
                );
            };
            if !checked.insert(path.to_string()) {
                continue;
            }
            // Empty stdout means clean OR not a git repo (git's complaint goes
            // to stderr) — both reapable, matching the rail's exact wording:
            // "skip if cwd IS a git repo WITH a dirty worktree".
            match self
                .runner
                .run("git", &["-C", path, "status", "--porcelain"])
            {
                Ok(status) if !status.trim().is_empty() => {
                    return spare(
                        Some(path.to_string()),
                        format!(
                            "dirty git worktree at {path} — an unstaged fix is not \
                             recoverable from tmux scrollback"
                        ),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    return spare(
                        Some(path.to_string()),
                        format!("cannot check {path} for uncommitted work ({e})"),
                    );
                }
            }
        }
        None
    }

    // ---- reap_orphans ---------------------------------------------------------

    pub fn preview_orphans(&self) -> Result<OrphanReapReport> {
        Ok(OrphanReapReport {
            action: "reapOrphans".into(),
            executed: false,
            term_wait_secs: self.term_wait.as_secs(),
            candidates: self.orphan_candidates()?,
        })
    }

    /// SIGTERM → wait → RECOMPUTE → SIGKILL the verified survivors.
    pub fn execute_orphans(&self) -> Result<OrphanReapReport> {
        // Recomputed HERE, at execution time. `perf-scan`'s own comment: the
        // report above may have taken long enough for the set to shift, and
        // killing a recycled PID is unacceptable.
        let mut candidates = self.orphan_candidates()?;

        if !candidates.is_empty() {
            let pids: Vec<String> = candidates.iter().map(|c| c.pid.to_string()).collect();
            let mut term_args = vec!["-TERM"];
            term_args.extend(pids.iter().map(String::as_str));
            self.runner.run("kill", &term_args)?;

            std::thread::sleep(self.term_wait);

            // The survivor check is a THIRD reading, not `kill -0`: a pid still
            // present must ALSO still match the orphan patterns, still have
            // ppid 1, and still report the same executable before SIGKILL will
            // touch it. A recycled pid fails that test three ways.
            let fresh: std::collections::HashMap<u32, String> = self
                .orphan_candidates()?
                .into_iter()
                .map(|c| (c.pid, c.program))
                .collect();

            let mut kill_args = vec!["-KILL".to_string()];
            for c in &mut candidates {
                if fresh.get(&c.pid) == Some(&c.program) {
                    c.outcome = Some("killed".into());
                    kill_args.push(c.pid.to_string());
                } else {
                    c.outcome = Some("terminated".into());
                }
            }
            if kill_args.len() > 1 {
                let args: Vec<&str> = kill_args.iter().map(String::as_str).collect();
                self.runner.run("kill", &args)?;
            }
        }

        Ok(OrphanReapReport {
            action: "reapOrphans".into(),
            executed: true,
            term_wait_secs: self.term_wait.as_secs(),
            candidates,
        })
    }

    /// The one selection function: fresh `ps`, the census's own pattern match,
    /// ppid 1. There is deliberately no variant taking a pid list.
    fn orphan_candidates(&self) -> Result<Vec<OrphanCandidate>> {
        let core = self
            .runner
            .run("ps", &["-Ao", "pid=,ppid=,rss=,etime=,time=,tty=,comm="])?;
        let args_out = self.runner.run("ps", &["-Ao", "pid=,args="])?;
        let rows = parse::merge_ps(
            parse::parse_ps_core(&core),
            &parse::parse_ps_args(&args_out),
        );

        let mut out: Vec<OrphanCandidate> = rows
            .iter()
            .filter(|r| r.ppid == 1 && self.config.is_orphan_candidate(&r.args))
            .map(|r| OrphanCandidate {
                pid: r.pid,
                program: r.program().to_string(),
                args: r.args.clone(),
                rss_bytes: r.rss_bytes(),
                age_secs: r.etime_secs,
                outcome: None,
            })
            .collect();
        // Stable order for a byte-comparable report.
        out.sort_by_key(|c| c.pid);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::census::exec::testing::FakeRunner;

    const PS_CORE: &str = include_str!("../../../tests/fixtures/census/ps-core.txt");
    const PS_ARGS: &str = include_str!("../../../tests/fixtures/census/ps-args.txt");

    const PS_CORE_CMD: &str = "ps -Ao pid=,ppid=,rss=,etime=,time=,tty=,comm=";
    const PS_ARGS_CMD: &str = "ps -Ao pid=,args=";
    const TMUX_CMD: &str = "tmux list-panes -a -F #{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}|#{pane_current_path}";

    /// Fixed "now". The stale sessions below have activity 3+ days earlier.
    const NOW: i64 = 1_788_190_000;
    const THREE_DAYS_AGO: i64 = NOW - 3 * 86_400 - 40_000;
    const RECENT: i64 = NOW - 600;

    /// One session per rail, plus a fresh one that must not appear at all.
    /// Every property that could co-vary is separated: the busy session is
    /// STALE (so the staleness filter alone cannot spare it), the dirty and
    /// clean sessions differ ONLY in their `git status` output, and the
    /// capitalized `Python` is exactly what tmux reports.
    fn tmux_fixture() -> String {
        format!(
            "fresh|100|zsh|{RECENT}|/tmp/fresh\n\
             busy-old|200|Python|{THREE_DAYS_AGO}|/tmp/proj-busy\n\
             dirty-old|300|zsh|{THREE_DAYS_AGO}|/tmp/proj-dirty\n\
             clean-old|400|zsh|{THREE_DAYS_AGO}|/tmp/proj-clean\n\
             undated|500|zsh||/tmp/undated\n"
        )
    }

    fn runner() -> FakeRunner {
        FakeRunner::new()
            .with(TMUX_CMD, &tmux_fixture())
            .with(
                "git -C /tmp/proj-dirty status --porcelain",
                " M src/main.rs\n",
            )
            .with("git -C /tmp/proj-clean status --porcelain", "")
            .with(PS_CORE_CMD, PS_CORE)
            .with(PS_ARGS_CMD, PS_ARGS)
    }

    fn actions(r: FakeRunner) -> ActionRunner {
        ActionRunner::new(Box::new(r), CensusConfig::default()).with_term_wait(Duration::ZERO)
    }

    /// For tests that assert on the CALLS: the fake stays in the test's hands
    /// via `Arc` (a shared runner is a runner — see `exec.rs`).
    fn actions_shared(r: FakeRunner) -> (std::sync::Arc<FakeRunner>, ActionRunner) {
        let r = std::sync::Arc::new(r);
        let a = ActionRunner::new(Box::new(std::sync::Arc::clone(&r)), CensusConfig::default())
            .with_term_wait(Duration::ZERO);
        (r, a)
    }

    fn by<'a>(report: &'a SessionReapReport, name: &str) -> &'a SessionCandidate {
        report
            .candidates
            .iter()
            .find(|c| c.session == name)
            .unwrap_or_else(|| panic!("no candidate {name}: {:?}", report.candidates))
    }

    // ---- session rails ------------------------------------------------------

    /// Rail 3, with the case that bites: tmux reports `Python`, capitalized,
    /// and the session is STALE — a fixture where the busy pane were also
    /// fresh would let the staleness filter alone pass this test (the
    /// recurring co-varying-fixture shape). Mutation-proof: drop the busy
    /// check from `session_candidates` and `busy-old` becomes a reap.
    #[test]
    fn a_capitalized_busy_pane_spares_a_stale_session() {
        let report = actions(runner()).preview_stale_sessions_at(NOW).unwrap();
        let busy = by(&report, "busy-old");
        assert_eq!(busy.verdict, SessionVerdict::Spare);
        assert_eq!(busy.pane_command, "Python", "raw capitalization preserved");
        assert!(
            busy.reason.contains("busy pane (Python)"),
            "{}",
            busy.reason
        );
        assert!(busy.idle_days.unwrap() > 3.0, "must be genuinely stale");
    }

    /// Rail 4: `dirty-old` and `clean-old` are identical in every respect —
    /// same age, same idle shell — except what `git status --porcelain`
    /// prints. Mutation-proof: invert the `is_empty` test and the two swap.
    #[test]
    fn a_dirty_worktree_spares_a_stale_session_and_a_clean_one_does_not() {
        let report = actions(runner()).preview_stale_sessions_at(NOW).unwrap();

        let dirty = by(&report, "dirty-old");
        assert_eq!(dirty.verdict, SessionVerdict::Spare);
        assert!(
            dirty
                .reason
                .contains("dirty git worktree at /tmp/proj-dirty"),
            "{}",
            dirty.reason
        );
        assert_eq!(dirty.cwd.as_deref(), Some("/tmp/proj-dirty"));

        let clean = by(&report, "clean-old");
        assert_eq!(clean.verdict, SessionVerdict::Reap, "{}", clean.reason);
    }

    /// Rail 1: unknown age appears as spare (so the person sees why), and a
    /// fresh session does not appear at all.
    #[test]
    fn unknown_age_is_spared_and_fresh_sessions_are_not_candidates() {
        let report = actions(runner()).preview_stale_sessions_at(NOW).unwrap();

        let undated = by(&report, "undated");
        assert_eq!(undated.verdict, SessionVerdict::Spare);
        assert_eq!(undated.idle_days, None);
        assert!(
            undated.reason.contains("cannot be dated"),
            "{}",
            undated.reason
        );

        assert!(
            !report.candidates.iter().any(|c| c.session == "fresh"),
            "a fresh session is not a candidate: {:?}",
            report.candidates
        );
    }

    /// If git itself cannot run, the session is spared: "we could not check"
    /// is never "safe to kill" — the same shape as rail 1.
    #[test]
    fn an_unrunnable_git_spares_the_session() {
        let report = actions(runner().unavailable("git"))
            .preview_stale_sessions_at(NOW)
            .unwrap();
        for name in ["dirty-old", "clean-old"] {
            let c = by(&report, name);
            assert_eq!(c.verdict, SessionVerdict::Spare, "{name}");
            assert!(c.reason.contains("cannot check"), "{}", c.reason);
        }
    }

    /// A pane with no reported cwd cannot be checked for unsaved work.
    #[test]
    fn a_missing_pane_cwd_spares_the_session() {
        let r = FakeRunner::new().with(TMUX_CMD, &format!("pathless|600|zsh|{THREE_DAYS_AGO}|\n"));
        let report = actions(r).preview_stale_sessions_at(NOW).unwrap();
        let c = by(&report, "pathless");
        assert_eq!(c.verdict, SessionVerdict::Spare);
        assert!(
            c.reason.contains("working directory unknown"),
            "{}",
            c.reason
        );
    }

    /// `kill-session` takes the WHOLE session, so one busy pane must spare a
    /// session whose other panes are idle. Distinguishing fixture: the same
    /// session name appears twice, once idle and once mid-cargo.
    #[test]
    fn one_busy_pane_spares_a_whole_multi_pane_session() {
        let r = FakeRunner::new().with(
            TMUX_CMD,
            &format!(
                "multi|700|zsh|{THREE_DAYS_AGO}|/tmp/multi\n\
                 multi|701|cargo|{THREE_DAYS_AGO}|/tmp/multi\n"
            ),
        );
        let report = actions(r).preview_stale_sessions_at(NOW).unwrap();
        assert_eq!(report.candidates.len(), 1, "one session, not two panes");
        let c = by(&report, "multi");
        assert_eq!(c.verdict, SessionVerdict::Spare);
        assert!(c.reason.contains("busy pane (cargo)"), "{}", c.reason);
    }

    // ---- session execute ------------------------------------------------------

    /// Execute kills exactly the reap verdicts, with tmux's EXACT-match `=`
    /// prefix. Mutation-proof twice: drop the verdict filter and the spared
    /// sessions gain kill calls; drop the `=` and the target string changes
    /// (prefix matching would take `clean-old-important` along with
    /// `clean-old`).
    #[test]
    fn execute_kills_only_reap_verdicts_and_pins_the_exact_name() {
        let (r, a) = actions_shared(runner());
        let report = a.execute_stale_sessions_at(NOW).unwrap();

        assert!(report.executed);
        let kills: Vec<String> = r
            .calls_to("tmux")
            .into_iter()
            .filter(|c| c.contains("kill-session"))
            .collect();
        assert_eq!(
            kills,
            vec!["tmux kill-session -t =clean-old".to_string()],
            "exactly one kill, exact-match target"
        );

        // The re-list replays the same fixture (the fake does not delete), so
        // the honest outcome here is "survived" — which also pins the
        // verify-by-relisting behaviour.
        assert_eq!(
            by(&report, "clean-old").outcome.as_deref(),
            Some("survived")
        );
        assert_eq!(by(&report, "busy-old").outcome, None, "spared: no outcome");
    }

    /// When the re-list shows the session gone, the outcome is "killed".
    #[test]
    fn a_session_absent_from_the_relist_is_reported_killed() {
        let after = format!(
            "fresh|100|zsh|{RECENT}|/tmp/fresh\n\
             busy-old|200|Python|{THREE_DAYS_AGO}|/tmp/proj-busy\n\
             dirty-old|300|zsh|{THREE_DAYS_AGO}|/tmp/proj-dirty\n\
             undated|500|zsh||/tmp/undated\n"
        );
        let r = FakeRunner::new()
            .with_sequence(TMUX_CMD, &[&tmux_fixture(), &after])
            .with(
                "git -C /tmp/proj-dirty status --porcelain",
                " M src/main.rs\n",
            )
            .with("git -C /tmp/proj-clean status --porcelain", "");
        let report = actions(r).execute_stale_sessions_at(NOW).unwrap();
        assert_eq!(by(&report, "clean-old").outcome.as_deref(), Some("killed"));
    }

    /// A preview never spawns a kill. Mutation-proof: route preview through
    /// the execute path and this sees the kill-session call.
    #[test]
    fn a_session_preview_kills_nothing() {
        let (r, a) = actions_shared(runner());
        let report = a.preview_stale_sessions_at(NOW).unwrap();
        assert!(!report.executed);
        assert!(by(&report, "clean-old").outcome.is_none());
        assert!(
            !r.calls().iter().any(|c| c.contains("kill")),
            "preview must not kill: {:?}",
            r.calls()
        );
    }

    /// tmux unreachable is "could not look", never an empty success.
    #[test]
    fn a_missing_tmux_is_reported_as_unavailable() {
        let report = actions(runner().unavailable("tmux"))
            .preview_stale_sessions_at(NOW)
            .unwrap();
        assert!(!report.tmux_available);
        assert!(report.candidates.is_empty());
    }

    // ---- orphans ---------------------------------------------------------------

    /// Selection matches the census's rules: pattern + ppid 1. The fixture's
    /// 33003 and 44002 match the patterns but have LIVE parents — the rows
    /// that distinguish "orphaned helper" from "helper".
    #[test]
    fn orphan_selection_requires_both_the_pattern_and_ppid_one() {
        let report = actions(runner()).preview_orphans().unwrap();
        let pids: Vec<u32> = report.candidates.iter().map(|c| c.pid).collect();
        assert_eq!(pids, vec![33001, 33002, 44001], "sorted, orphans only");
        assert!(!report.executed);
        assert!(report.candidates.iter().all(|c| c.outcome.is_none()));
    }

    /// THE acceptance criterion: execute recomputes, and the post-wait SIGKILL
    /// list is a third reading. Between the TERM reading and the survivor
    /// reading, 33001 exits and 33002's pid is RECYCLED by a different
    /// program that also matches the orphan patterns (`other-mcp-server`,
    /// ppid 1 — the worst case). Only 44001, still itself, may be SIGKILLed.
    ///
    /// Mutation-proof three ways: SIGKILL the original list → 33001 and 33002
    /// appear in the kill call; match survivors by pid alone → 33002 is
    /// SIGKILLed despite being a different program; skip the recompute → same.
    #[test]
    fn execute_orphans_sigkills_only_reverified_survivors() {
        let after_core = "\
33002     1  38912    00:02   0:00.01 ??       other-mcp-server
44001     1  22528 02:00:00   0:09.00 ??       rust-language-server
";
        let after_args = "\
33002 /usr/local/bin/other-mcp-server
44001 /Users/example/.local/share/nvim/mason/bin/rust-language-server
";
        let (r, a) = actions_shared(
            FakeRunner::new()
                .with(TMUX_CMD, &tmux_fixture())
                .with_sequence(PS_CORE_CMD, &[PS_CORE, after_core])
                .with_sequence(PS_ARGS_CMD, &[PS_ARGS, after_args]),
        );
        let report = a.execute_orphans().unwrap();

        let kills = r.calls_to("kill");
        assert_eq!(
            kills,
            vec![
                "kill -TERM 33001 33002 44001".to_string(),
                "kill -KILL 44001".to_string(),
            ],
            "TERM the first reading; KILL only the reverified survivor"
        );

        let outcome = |pid: u32| {
            report
                .candidates
                .iter()
                .find(|c| c.pid == pid)
                .unwrap()
                .outcome
                .clone()
        };
        assert_eq!(outcome(33001).as_deref(), Some("terminated"));
        assert_eq!(
            outcome(33002).as_deref(),
            Some("terminated"),
            "a recycled pid reads as terminated, and is never SIGKILLed"
        );
        assert_eq!(outcome(44001).as_deref(), Some("killed"));
    }

    /// No candidates → no kill spawns at all. Mutation-proof: make the TERM
    /// call unconditional and this sees `kill -TERM` with no pids.
    #[test]
    fn an_empty_orphan_list_spawns_no_kill() {
        let (r, a) = actions_shared(
            FakeRunner::new()
                .with(
                    PS_CORE_CMD,
                    "  1     0  14608 21:43:13  10:59.18 ??       /sbin/launchd\n",
                )
                .with(PS_ARGS_CMD, "  1 /sbin/launchd\n"),
        );
        let report = a.execute_orphans().unwrap();
        assert!(report.executed);
        assert!(report.candidates.is_empty());
        assert!(r.calls_to("kill").is_empty(), "{:?}", r.calls());
    }

    /// An orphan preview never spawns a kill.
    #[test]
    fn an_orphan_preview_kills_nothing() {
        let (r, a) = actions_shared(runner());
        a.preview_orphans().unwrap();
        assert!(r.calls_to("kill").is_empty());
    }

    /// A failing `ps` is an error, not an empty reap — the census's rule.
    #[test]
    fn a_failing_ps_is_an_error_not_an_empty_orphan_list() {
        assert!(
            actions(runner().unavailable("ps"))
                .preview_orphans()
                .is_err()
        );
    }

    // ---- wire shape ---------------------------------------------------------

    /// camelCase at every depth; nullable fields present-as-null (the encoder
    /// side of the contract). No timestamp — the payload must be byte-stable
    /// so the parity harness can compare it across surfaces.
    #[test]
    fn reports_serialize_with_camel_case_and_explicit_nulls() {
        let report = actions(runner()).preview_stale_sessions_at(NOW).unwrap();
        let v: serde_json::Value = serde_json::to_value(&report).unwrap();

        for key in [
            "action",
            "executed",
            "tmuxAvailable",
            "staleDays",
            "candidates",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(v["action"], "reapStaleSessions");
        let c = &v["candidates"][0];
        for key in [
            "session",
            "paneCommand",
            "idleDays",
            "cwd",
            "verdict",
            "reason",
            "outcome",
        ] {
            assert!(c.get(key).is_some(), "missing candidate key {key}: {c}");
        }
        assert!(
            v["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["outcome"].is_null()),
            "preview outcomes are explicit nulls"
        );
        let undated = v["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["session"] == "undated")
            .unwrap();
        assert!(
            undated["idleDays"].is_null(),
            "unknown age is an explicit null"
        );

        let verdicts: Vec<&str> = v["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["verdict"].as_str().unwrap())
            .collect();
        assert!(verdicts.contains(&"reap") && verdicts.contains(&"spare"));

        let o: serde_json::Value =
            serde_json::to_value(actions(runner()).preview_orphans().unwrap()).unwrap();
        assert_eq!(o["action"], "reapOrphans");
        assert_eq!(o["termWaitSecs"], 0);
        assert!(o["candidates"][0].get("rssBytes").is_some());
    }

    // ---- the shared fixtures (raw bytes, both languages) ----------------------

    #[test]
    fn decodes_the_reap_sessions_fixture_from_raw_bytes() {
        let raw = include_str!("../../../tests/fixtures/reap-sessions.json");
        let r: SessionReapReport = serde_json::from_str(raw).unwrap();
        assert!(r.executed);
        assert_eq!(r.stale_days, 2);
        assert_eq!(r.candidates.len(), 4);
        assert_eq!(r.candidates[0].verdict, SessionVerdict::Reap);
        assert_eq!(r.candidates[0].outcome.as_deref(), Some("killed"));
        // The busy candidate is the capitalized-Python case, stale AND busy.
        let busy = &r.candidates[1];
        assert_eq!(busy.verdict, SessionVerdict::Spare);
        assert_eq!(busy.pane_command, "Python");
        assert!(busy.idle_days.unwrap() > 2.0);
        assert_eq!(busy.outcome, None, "spared sessions have null outcomes");
        // The undated candidate: null idleDays is "unknown", not zero.
        assert_eq!(r.candidates[3].idle_days, None);
    }

    #[test]
    fn decodes_the_reap_orphans_fixture_from_raw_bytes() {
        let raw = include_str!("../../../tests/fixtures/reap-orphans.json");
        let r: OrphanReapReport = serde_json::from_str(raw).unwrap();
        assert!(r.executed);
        assert_eq!(r.term_wait_secs, 8);
        assert_eq!(r.candidates.len(), 2);
        assert_eq!(r.candidates[0].outcome.as_deref(), Some("terminated"));
        assert_eq!(r.candidates[1].outcome.as_deref(), Some("killed"));
    }
}
