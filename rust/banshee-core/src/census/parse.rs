// census/parse — raw OS output in, structs out. No config, no classification,
// no side effects.
//
// This is where the census tier earns the testing standard's "parsers start from
// RAW BYTES" rule (which ADR-0007 exempted the syscall-based cheap tier from,
// because there are no bytes there). Every function here is driven from captured
// `ps` / `tmux` / `lsof` output in `tests/fixtures/census/`.
//
// Every trap below cost `perf-scan` something; see docs/signal-collection.md.

/// A process, assembled from TWO `ps` invocations.
///
/// Not `Eq`: `cpu_secs` is an `f64`.
///
/// **Why two calls.** We need both `comm` (the executable identity, which is
/// what `pgrep -x` matches) and `args` (the full command line, which app-group
/// and orphan-pattern matching test against). Both can contain spaces —
/// `/Applications/Google Chrome.app/.../Google Chrome Helper` — and two
/// variable-width space-containing fields on one line cannot be parsed
/// unambiguously. So:
///
/// - `ps -Ao pid=,ppid=,rss=,etime=,time=,tty=,comm=` — six fixed fields, comm last
/// - `ps -Ao pid=,args=` — one fixed field, args last
///
/// joined on pid. Two spawns per census instead of one, against `perf-scan`'s
/// roughly eight, at a 5-minute cadence (ADR-0007).
///
/// Using `args`'s argv[0] alone would have been one call, and it happens to work
/// for the agent CLIs measured on this machine (`claude` reports argv[0] as a
/// bare `claude`) — but argv[0] is whatever the process chose to put there, and
/// a wrapper script or an interpreter-launched tool makes it lie. `comm` is what
/// the kernel knows.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcRow {
    pub pid: u32,
    pub ppid: u32,
    pub rss_kb: u64,
    /// Wall-clock age. `[[DD-]HH:]MM:SS`.
    pub etime_secs: u64,
    /// Cumulative CPU time. **`MMM:SS.ss` on macOS — minutes are unbounded and
    /// never roll into hours** (measured: `315:45.00`).
    pub cpu_secs: f64,
    /// `??` for a process with no controlling terminal.
    pub tty: String,
    /// The executable, as the kernel reports it. May be a bare name or a full
    /// path, and may contain spaces.
    pub comm: String,
    /// Full command line. Empty when the second `ps` call had no row for this
    /// pid — i.e. the process exited between the two calls, which is normal.
    pub args: String,
}

impl ProcRow {
    /// True when nothing owns a terminal for this process.
    ///
    /// The load-bearing distinction in the whole census: no controlling terminal
    /// means an editor or app spawned it (an agent CLI under an IDE), not a session
    /// someone is sitting in. Without this split the session list is unusable —
    /// one measurement found 48 IDE-spawned helpers drowning the real sessions.
    pub fn is_ide_spawned(&self) -> bool {
        self.tty.is_empty() || self.tty == "??" || self.tty == "?"
    }

    /// The program identity `pgrep -x` matches: `comm`'s basename.
    ///
    /// Deliberately NOT derived from `args`. A substring test against the command
    /// line matches any process whose *arguments* mention the name — `rg --files
    /// claude`, an editor with the file open, this test binary. And argv[0] is
    /// whatever the process chose to write there, while `comm` is the kernel's
    /// own record.
    pub fn program(&self) -> &str {
        basename(&self.comm)
    }

    pub fn rss_bytes(&self) -> u64 {
        self.rss_kb.saturating_mul(1024)
    }

    /// Whole days of wall-clock age. The cheapest multi-day staleness test.
    pub fn age_days(&self) -> u64 {
        self.etime_secs / 86_400
    }

    /// Cumulative CPU as a percentage of ONE core over the process's lifetime.
    ///
    /// **This, not `%CPU`.** `%CPU` is instantaneous and lies about daemons. The
    /// 2026-08-04 measurement is the whole argument: the managed agents sat at
    /// the top of `top` and were a few percent of total capacity combined,
    /// while the actual cause was a pile of idle multi-day agent sessions.
    pub fn cpu_percent_of_one_core(&self) -> f64 {
        if self.etime_secs == 0 {
            // A process in its first second has no meaningful ratio, and
            // dividing would give inf — which compares false against every
            // threshold and would silently exempt it forever.
            return 0.0;
        }
        self.cpu_secs * 100.0 / self.etime_secs as f64
    }
}

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parse `ps -Ao pid=,ppid=,rss=,etime=,time=,tty=,comm=`.
///
/// Unparseable lines are SKIPPED rather than failing the census: `ps` output is a
/// snapshot of a moving system, and one weird row must not cost the whole
/// reading. A header line (from an invocation without the `=` suffixes) has no
/// numeric pid and is skipped by the same path.
pub fn parse_ps_core(output: &str) -> Vec<ProcRow> {
    output.lines().filter_map(parse_ps_core_line).collect()
}

fn parse_ps_core_line(line: &str) -> Option<ProcRow> {
    // Six fixed fields, then comm as the REMAINDER. comm contains spaces for
    // bundled apps, so the tail cannot be split on whitespace.
    let (fields, rest) = split_fixed(line, 6)?;

    Some(ProcRow {
        pid: fields[0].parse().ok()?,
        ppid: fields[1].parse().ok()?,
        rss_kb: fields[2].parse().ok()?,
        etime_secs: parse_etime(fields[3])?,
        cpu_secs: parse_cpu_time(fields[4])?,
        tty: fields[5].to_string(),
        comm: rest?.to_string(),
        args: String::new(),
    })
}

/// Parse `ps -Ao pid=,args=` into pid → command line.
pub fn parse_ps_args(output: &str) -> std::collections::HashMap<u32, String> {
    let mut map = std::collections::HashMap::new();
    for line in output.lines() {
        if let Some((fields, Some(args))) = split_fixed(line, 1)
            && let Ok(pid) = fields[0].parse::<u32>()
        {
            map.insert(pid, args.to_string());
        }
    }
    map
}

/// Join the two `ps` readings on pid.
///
/// A row present in the core reading but missing from the args reading keeps an
/// empty `args`: the process exited between the two calls. It is still a real
/// process that was running, so it stays in the census — dropping it would
/// understate counts, and counts are what the pressure model bands on.
pub fn merge_ps(
    mut core: Vec<ProcRow>,
    args: &std::collections::HashMap<u32, String>,
) -> Vec<ProcRow> {
    for row in &mut core {
        if let Some(a) = args.get(&row.pid) {
            row.args = a.clone();
        }
    }
    core
}

/// Split off `n` whitespace-delimited fields, returning them plus the remainder.
///
/// `ps` right-aligns its numeric columns, so runs of spaces are normal and the
/// naive `splitn` produces empty fields. Returns `None` when fewer than `n`
/// fields are present.
fn split_fixed(line: &str, n: usize) -> Option<(Vec<&str>, Option<&str>)> {
    let bytes = line.as_bytes();
    let mut fields = Vec::with_capacity(n);
    let mut i = 0usize;
    while fields.len() < n {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if start == i {
            return None;
        }
        fields.push(&line[start..i]);
    }
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let rest = if i < bytes.len() {
        Some(line[i..].trim_end())
    } else {
        None
    };
    Some((fields, rest))
}

/// Parse `ps` ELAPSED: `[[DD-]HH:]MM:SS`.
///
/// Traps:
/// - **A dash means the process is older than 24 hours** and the part before it
///   is whole days. This is the cheapest multi-day test available.
/// - **Leading zeros are decimal, not octal.** `08-04:00:00` is 8 days. In shell
///   this needed an explicit `10#` prefix; Rust's `parse` is already decimal, but
///   the case is pinned because getting it wrong once cost a real bug.
pub fn parse_etime(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, s),
    };

    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec) = match parts.len() {
        3 => (
            parts[0].parse::<u64>().ok()?,
            parts[1].parse::<u64>().ok()?,
            parts[2].parse::<u64>().ok()?,
        ),
        2 => (
            0,
            parts[0].parse::<u64>().ok()?,
            parts[1].parse::<u64>().ok()?,
        ),
        1 => (0, 0, parts[0].parse::<u64>().ok()?),
        _ => return None,
    };
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

/// Parse `ps` TIME (cumulative CPU): `MMM:SS.ss`.
///
/// **On macOS this never uses hours** — minutes are unbounded and just keep
/// counting (measured: `315:45.00`, i.e. 315 minutes, on a process whose ELAPSED
/// was `21:39:04`). The `HH:MM:SS` and `DD-HH:MM:SS` shapes are accepted anyway,
/// because `perf-scan`'s shared parser handled them and a defensive parser costs
/// nothing.
pub fn parse_cpu_time(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (days, hms) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<f64>().ok()?, rest),
        None => (0.0, s),
    };

    let parts: Vec<&str> = hms.split(':').collect();
    let secs = match parts.len() {
        3 => {
            parts[0].parse::<f64>().ok()? * 3600.0
                + parts[1].parse::<f64>().ok()? * 60.0
                + parts[2].parse::<f64>().ok()?
        }
        2 => parts[0].parse::<f64>().ok()? * 60.0 + parts[1].parse::<f64>().ok()?,
        1 => parts[0].parse::<f64>().ok()?,
        _ => return None,
    };
    Some(days * 86_400.0 + secs)
}

/// One pane from
/// `tmux list-panes -a -F "#{session_name}|#{pane_pid}|#{pane_current_command}|#{session_activity}"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxPane {
    pub session: String,
    pub pane_pid: u32,
    /// **tmux reports the binary's OWN name, capitalized for framework
    /// interpreters** — `Python`, not `python3`. Lowercase before matching a
    /// busy-command list or a running build will look idle and get killed.
    pub current_command: String,
    /// Unix seconds of last session activity. `0`/absent means unknown.
    pub activity_secs: i64,
    /// The pane's working directory (`#{pane_current_path}`), present only when
    /// the caller's format string asked for it. The census's does not; the
    /// action tier's does — the dirty-worktree rail needs somewhere to run
    /// `git`, and the census must not pay for a field only actions read.
    pub path: Option<String>,
}

impl TmuxPane {
    /// The command, lowercased, ready for busy-list matching.
    pub fn command_for_matching(&self) -> String {
        self.current_command.to_lowercase()
    }

    pub fn idle_days(&self, now_secs: i64) -> Option<f64> {
        if self.activity_secs <= 0 {
            // Unknown activity is not "idle forever" — a session we cannot date
            // must never be treated as reapable.
            return None;
        }
        Some((now_secs - self.activity_secs) as f64 / 86_400.0)
    }
}

pub fn parse_tmux_panes(output: &str) -> Vec<TmuxPane> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            // `tmux ls` on a machine with no server prints an error to stdout in
            // some versions ("no server running on ..."), which has no pipes.
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 4 {
                return None;
            }
            Some(TmuxPane {
                session: parts[0].to_string(),
                pane_pid: parts[1].parse().ok()?,
                current_command: parts[2].to_string(),
                // An empty or non-numeric activity is "unknown" (0), not a parse
                // failure — the pane is still real and still worth listing.
                activity_secs: parts[3].trim().parse().unwrap_or(0),
                path: parts
                    .get(4)
                    .map(|p| p.trim())
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Extract the cwd from `lsof -a -p <pid> -d cwd -Fn`.
///
/// The `-F` output is one field per line, prefixed by a field letter: `p<pid>`,
/// `fcwd`, `n<path>`. We want the `n` line. Returns `None` when the process has
/// gone (lsof prints nothing) — which is normal for a census of a moving system.
pub fn parse_lsof_cwd(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.strip_prefix('n'))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS_CORE: &str = include_str!("../../../../tests/fixtures/census/ps-core.txt");
    const PS_ARGS: &str = include_str!("../../../../tests/fixtures/census/ps-args.txt");

    /// The two `ps` readings, joined — what the census actually works from.
    fn rows() -> Vec<ProcRow> {
        merge_ps(parse_ps_core(PS_CORE), &parse_ps_args(PS_ARGS))
    }
    const TMUX_FIXTURE: &str = include_str!("../../../../tests/fixtures/census/tmux-panes.txt");
    const LSOF_FIXTURE: &str = include_str!("../../../../tests/fixtures/census/lsof-cwd.txt");

    // ---- etime -----------------------------------------------------------

    #[test]
    fn parses_every_etime_shape_ps_emits() {
        assert_eq!(parse_etime("00:33"), Some(33));
        assert_eq!(parse_etime("05:50"), Some(350));
        assert_eq!(parse_etime("21:39:04"), Some(21 * 3600 + 39 * 60 + 4));
        assert_eq!(
            parse_etime("2-03:04:05"),
            Some(2 * 86400 + 3 * 3600 + 4 * 60 + 5)
        );
        assert_eq!(
            parse_etime("  00:33  "),
            Some(33),
            "ps right-aligns columns"
        );
    }

    /// A dash is the multi-day marker, and the day field must be read as
    /// DECIMAL. In shell this needed `10#` because `08` is invalid octal; the
    /// case is pinned here so a future port to another language re-learns it from
    /// a failing test rather than from production.
    #[test]
    fn a_leading_zero_day_count_is_decimal_not_octal() {
        assert_eq!(parse_etime("08-04:00:00"), Some(8 * 86400 + 4 * 3600));
        assert_eq!(parse_etime("09-00:00:00"), Some(9 * 86400));
        assert_eq!(parse_etime("017-00:00:00"), Some(17 * 86400));
    }

    #[test]
    fn rejects_unparseable_etime() {
        for bad in ["", "   ", "abc", "1:2:3:4", "--", "x-01:00:00", "01:xx"] {
            assert_eq!(parse_etime(bad), None, "must reject {bad:?}");
        }
    }

    // ---- cumulative CPU time --------------------------------------------

    /// macOS `ps -o time` does NOT roll minutes into hours. Mutation-proof:
    /// treat a 2-part value as `HH:MM` and 315:45.00 becomes 1,136,700 s
    /// (13 days of CPU on a machine up 21 hours) instead of 18,945 s.
    #[test]
    fn cumulative_cpu_minutes_are_unbounded_not_hours() {
        // Real values captured from this machine.
        assert_eq!(parse_cpu_time("315:45.00"), Some(315.0 * 60.0 + 45.0));
        assert_eq!(parse_cpu_time("82:36.81"), Some(82.0 * 60.0 + 36.81));
        assert_eq!(parse_cpu_time("10:54.89"), Some(10.0 * 60.0 + 54.89));
        assert_eq!(parse_cpu_time("0:00.01"), Some(0.01));
    }

    #[test]
    fn accepts_the_hour_and_day_cpu_shapes_defensively() {
        assert_eq!(parse_cpu_time("1:02:03"), Some(3723.0));
        assert_eq!(parse_cpu_time("2-01:00:00"), Some(2.0 * 86400.0 + 3600.0));
    }

    #[test]
    fn rejects_unparseable_cpu_time() {
        for bad in ["", "  ", "nope", "1:2:3:4"] {
            assert_eq!(parse_cpu_time(bad), None, "must reject {bad:?}");
        }
    }

    // ---- ps rows ---------------------------------------------------------

    #[test]
    fn parses_the_captured_ps_output() {
        let rows = rows();
        assert!(rows.len() >= 21, "got {} rows", rows.len());

        let launchd = rows.iter().find(|r| r.pid == 1).expect("pid 1");
        assert_eq!(launchd.ppid, 0);
        assert_eq!(launchd.rss_kb, 14608);
        assert_eq!(launchd.etime_secs, 21 * 3600 + 43 * 60 + 13);
        assert!((launchd.cpu_secs - (10.0 * 60.0 + 59.18)).abs() < 1e-9);
        assert_eq!(launchd.tty, "??");
        assert_eq!(launchd.comm, "/sbin/launchd");
        assert_eq!(launchd.args, "/sbin/launchd");
        assert_eq!(launchd.program(), "launchd");
    }

    /// `comm` contains spaces for bundled apps, so the tail of the core line
    /// cannot be split on whitespace. Mutation-proof: take `comm` as a single
    /// whitespace field and this truncates to "/Applications/Google".
    #[test]
    fn a_comm_containing_spaces_survives_intact() {
        let rows = rows();
        let chrome = rows.iter().find(|r| r.pid == 66001).expect("chrome helper");
        assert!(
            chrome.comm.starts_with("/Applications/Google Chrome.app/"),
            "comm truncated: {:?}",
            chrome.comm
        );
        assert!(chrome.comm.ends_with("Google Chrome Helper"));
        assert_eq!(chrome.program(), "Google Chrome Helper");
    }

    /// Same hazard on the args side, plus the trailing flag.
    #[test]
    fn an_args_line_containing_spaces_survives_intact() {
        let rows = rows();
        let chrome = rows.iter().find(|r| r.pid == 66001).expect("chrome helper");
        assert!(chrome.args.contains("Google Chrome Helper"));
        assert!(chrome.args.ends_with("--type=renderer"));
    }

    /// The two readings are joined on pid, not on position — the second `ps`
    /// call sees a different, moving process table. Mutation-proof: zip the two
    /// lists by index and the Chrome helper's args land on some other pid.
    #[test]
    fn the_two_ps_readings_join_on_pid() {
        let mut core = parse_ps_core(PS_CORE);
        // Shuffle the core order so an index-based join would visibly misalign.
        core.reverse();
        let merged = merge_ps(core, &parse_ps_args(PS_ARGS));

        for r in &merged {
            if r.pid == 2833 {
                assert!(r.args.starts_with("claude --model"), "got {:?}", r.args);
            }
            if r.pid == 91002 {
                assert_eq!(r.args, "kiro-cli --resume");
            }
        }
    }

    /// A process that exits between the two `ps` calls keeps an empty `args` and
    /// STAYS in the census. Dropping it would understate the counts the pressure
    /// model bands on. Mutation-proof: filter unmatched rows out of `merge_ps`
    /// and this fails.
    #[test]
    fn a_process_missing_from_the_args_reading_is_kept_with_empty_args() {
        let core = parse_ps_core(PS_CORE);
        let n = core.len();
        let merged = merge_ps(core, &std::collections::HashMap::new());
        assert_eq!(merged.len(), n, "no row may be dropped");
        assert!(merged.iter().all(|r| r.args.is_empty()));
        // And identity still works, because it comes from comm.
        assert!(merged.iter().any(|r| r.program() == "claude"));
    }

    /// A malformed row must not cost the whole census.
    #[test]
    fn unparseable_rows_are_skipped_not_fatal() {
        let input = "    1     0  14608 21:43:13  10:59.18 ??       /sbin/launchd\n\
                     garbage line with no numbers at all\n\
                     PID PPID RSS ELAPSED TIME TTY COMM\n\
                     \n\
                   123     1    456    00:10   0:00.50 ttys001  zsh\n";
        let rows = parse_ps_core(input);
        assert_eq!(rows.len(), 2, "the two valid rows survive");
        assert_eq!(rows[0].pid, 1);
        assert_eq!(rows[1].pid, 123);
    }

    #[test]
    fn parse_ps_on_empty_input_is_empty_not_a_panic() {
        assert!(parse_ps_core("").is_empty());
        assert!(parse_ps_core("\n\n  \n").is_empty());
        assert!(parse_ps_args("").is_empty());
    }

    /// A core row with no comm at all (six fields and nothing after) must not
    /// become a row with an empty identity that then matches a config entry.
    #[test]
    fn a_row_with_no_comm_is_skipped() {
        assert!(parse_ps_core("  1  0  100  00:10  0:00.10 ??").is_empty());
    }

    // ---- the interactive / IDE-spawned split ----------------------------

    /// The most consequential classification in the census.
    /// Mutation-proof: invert `is_ide_spawned` and the fixture's real sessions
    /// and helpers swap places.
    #[test]
    fn processes_without_a_controlling_terminal_are_ide_spawned() {
        let rows = rows();
        let claudes: Vec<&ProcRow> = rows.iter().filter(|r| r.program() == "claude").collect();
        assert!(claudes.len() >= 4, "fixture needs several claude rows");

        let interactive = claudes.iter().filter(|r| !r.is_ide_spawned()).count();
        let helpers = claudes.iter().filter(|r| r.is_ide_spawned()).count();
        assert!(interactive >= 2, "expected interactive sessions");
        assert!(helpers >= 1, "expected at least one IDE-spawned helper");

        for r in &claudes {
            assert_eq!(
                r.is_ide_spawned(),
                r.tty == "??",
                "pid {} tty {:?}",
                r.pid,
                r.tty
            );
        }
    }

    #[test]
    fn an_empty_tty_counts_as_ide_spawned() {
        let row = ProcRow {
            pid: 1,
            ppid: 1,
            rss_kb: 0,
            etime_secs: 1,
            cpu_secs: 0.0,
            tty: String::new(),
            comm: "x".into(),
            args: "/x".into(),
        };
        assert!(row.is_ide_spawned());
    }

    // ---- program identification -----------------------------------------

    /// `pgrep -x` matches the program name exactly. A substring test against the
    /// full command line would match any process whose ARGUMENTS mention the
    /// name — `grep claude`, an editor with the file open, this very test binary.
    /// Mutation-proof: make `program()` return `self.args` and this fails.
    #[test]
    fn program_is_argv0s_basename_not_a_substring_of_the_command_line() {
        // Real shape: `rg --files claude` — the command line mentions claude,
        // the program is not claude.
        let row = rows()
            .into_iter()
            .find(|r| r.pid == 99100)
            .expect("the rg row");
        assert!(row.args.contains("claude"), "args must mention claude");
        assert_eq!(row.program(), "rg", "must not be fooled by an argument");
        assert_ne!(row.program(), "claude");
    }

    #[test]
    fn basename_handles_bare_names_and_trailing_paths() {
        assert_eq!(basename("/usr/bin/env"), "env");
        assert_eq!(basename("claude"), "claude");
        assert_eq!(basename(""), "");
        assert_eq!(basename("/a/b/"), "");
    }

    // ---- cumulative CPU ratio -------------------------------------------

    /// The honest cost metric. Mutation-proof: multiply by 1.0 instead of 100.0
    /// and every monitoring agent reads as ~0.2% instead of ~20%.
    #[test]
    fn cpu_percent_is_cumulative_time_over_lifetime() {
        let row = ProcRow {
            pid: 5,
            ppid: 1,
            rss_kb: 0,
            // 176:45.22 of CPU over 21:39:04 of life — the shape of a long-lived
            // managed agent.
            etime_secs: 21 * 3600 + 39 * 60 + 4,
            cpu_secs: 176.0 * 60.0 + 45.22,
            tty: "??".into(),
            comm: "com.example.endpointagent".into(),
            args: "/Library/SystemExtensions/example-agent".into(),
        };
        let pct = row.cpu_percent_of_one_core();
        assert!((pct - 13.6).abs() < 0.2, "got {pct}");
    }

    /// A brand-new process has no meaningful ratio, and dividing by zero gives
    /// inf — which compares false against every threshold and would exempt the
    /// process forever. Mutation-proof: drop the guard and this fails on
    /// `is_finite`.
    #[test]
    fn a_zero_lifetime_does_not_produce_infinity() {
        let row = ProcRow {
            pid: 5,
            ppid: 1,
            rss_kb: 0,
            etime_secs: 0,
            cpu_secs: 5.0,
            tty: "??".into(),
            comm: "x".into(),
            args: "/x".into(),
        };
        assert!(row.cpu_percent_of_one_core().is_finite());
        assert_eq!(row.cpu_percent_of_one_core(), 0.0);
    }

    #[test]
    fn age_in_days_comes_from_elapsed_seconds() {
        let mk = |etime_secs| ProcRow {
            pid: 1,
            ppid: 1,
            rss_kb: 0,
            etime_secs,
            cpu_secs: 0.0,
            tty: "ttys001".into(),
            comm: "x".into(),
            args: "/x".into(),
        };
        assert_eq!(mk(0).age_days(), 0);
        assert_eq!(mk(86_399).age_days(), 0);
        assert_eq!(mk(86_400).age_days(), 1);
        assert_eq!(mk(6 * 86_400 + 100).age_days(), 6);
    }

    // ---- tmux ------------------------------------------------------------

    #[test]
    fn parses_the_captured_tmux_output() {
        let panes = parse_tmux_panes(TMUX_FIXTURE);
        assert!(panes.len() >= 4, "got {}", panes.len());

        let first = &panes[0];
        assert_eq!(first.session, "banshee-p1-worker");
        assert_eq!(first.pane_pid, 2833);
        assert_eq!(first.current_command, "zsh");
        assert_eq!(first.activity_secs, 1_788_190_000);
    }

    /// tmux reports framework interpreters CAPITALIZED (`Python`, not
    /// `python3`), so a busy-command list must be matched against the lowercased
    /// form. Getting this wrong means a session running a build looks idle and
    /// gets killed. Mutation-proof: have `command_for_matching` return the raw
    /// string and this fails.
    #[test]
    fn a_capitalized_interpreter_lowercases_for_matching() {
        let panes = parse_tmux_panes(TMUX_FIXTURE);
        let py = panes
            .iter()
            .find(|p| p.current_command == "Python")
            .expect("fixture must contain a capitalized Python pane");
        assert_eq!(py.command_for_matching(), "python");
        assert_ne!(py.command_for_matching(), "Python");
    }

    /// A session whose activity timestamp is unknown must NOT read as
    /// infinitely idle — that is the difference between "we don't know" and
    /// "safe to kill". Mutation-proof: return `Some(...)` for `activity_secs
    /// <= 0` and this fails.
    #[test]
    fn a_session_with_unknown_activity_has_no_idle_age() {
        let panes = parse_tmux_panes(TMUX_FIXTURE);
        let unknown = panes
            .iter()
            .find(|p| p.activity_secs == 0)
            .expect("fixture must contain a pane with no activity timestamp");
        assert_eq!(unknown.idle_days(1_788_190_000), None);
    }

    #[test]
    fn computes_idle_days_from_activity() {
        let pane = TmuxPane {
            session: "s".into(),
            pane_pid: 1,
            current_command: "zsh".into(),
            activity_secs: 1_788_000_000,
            path: None,
        };
        let now = 1_788_000_000 + 3 * 86_400;
        let d = pane.idle_days(now).unwrap();
        assert!((d - 3.0).abs() < 1e-9);
    }

    /// `tmux list-panes` against a machine with no server prints a plain error
    /// line. It must parse to nothing rather than to a bogus pane.
    #[test]
    fn tmux_error_output_yields_no_panes() {
        assert!(parse_tmux_panes("no server running on /private/tmp/tmux-503/default").is_empty());
        assert!(parse_tmux_panes("").is_empty());
        assert!(parse_tmux_panes("only|three|fields").is_empty());
    }

    /// A session name containing the delimiter must not silently become a
    /// different session. Session names come from tooling (`<project>-<task>-<worker>`),
    /// so this is unlikely — but a wrong session name means reaping the wrong thing.
    #[test]
    fn extra_delimiters_do_not_shift_the_fields() {
        let panes = parse_tmux_panes("a|123|zsh|1788000000|extra");
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].session, "a");
        assert_eq!(panes[0].pane_pid, 123);
        assert_eq!(panes[0].current_command, "zsh");
        assert_eq!(panes[0].activity_secs, 1_788_000_000);
    }

    // ---- lsof ------------------------------------------------------------

    #[test]
    fn extracts_the_cwd_from_lsof_field_output() {
        assert_eq!(
            parse_lsof_cwd(LSOF_FIXTURE).as_deref(),
            Some("/Users/example/Code/banshee")
        );
    }

    /// A process that exited between the census and the `lsof` call produces no
    /// output. That is normal, not an error.
    #[test]
    fn a_vanished_process_has_no_cwd() {
        assert_eq!(parse_lsof_cwd(""), None);
        assert_eq!(parse_lsof_cwd("p1649\nfcwd\n"), None, "no n-line");
        assert_eq!(parse_lsof_cwd("p1649\nfcwd\nn\n"), None, "empty path");
    }
}
