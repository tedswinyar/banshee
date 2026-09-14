// census/config — the five pattern lists the census classifies against, plus the
// machine-local overlay that keeps site-specific process names out of this
// repository.
//
// The overlay is operator-authored — banshee only READS it, never writes it, so
// it cannot set the file's permissions. On a SHARED Mac, `chmod 600` it: the
// names it holds are exactly the sensitive strings it exists to keep private,
// and its default 0644 lets other local users read them (docs/threat-model.md §4).
//
// `perf-scan` keeps these as bash arrays and sources `~/.config/perf-scan.local`
// after the defaults, relying on shell syntax to express intent:
// `MONITOR_AGENTS=(...)` REPLACES, `APP_GROUPS+=(...)` APPENDS. TOML has no such
// syntax, so each list is spelled twice: `x` replaces, `x_add` appends. Explicit
// beats clever here — a reader of the file should not have to know which lists
// historically replaced and which appended.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{CoreError, Result};

/// Default staleness threshold in days, matching `perf-scan`'s `TMUX_STALE_DAYS`.
pub const DEFAULT_STALE_DAYS: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusConfig {
    /// Agent CLI program names, matched EXACTLY against `comm`'s basename —
    /// `pgrep -x` semantics.
    pub agent_clis: Vec<String>,
    /// App names, matched as a case-sensitive substring of the full command line.
    /// Over-matches by design: any process whose argv mentions "Docker" counts,
    /// because multi-process apps hide their real footprint.
    pub app_groups: Vec<String>,
    /// Managed monitoring/security agent names, matched case-insensitively
    /// against the command line.
    ///
    /// **The committed default is deliberately generic.** Real names live only in
    /// the machine-local overlay; see the module docs and
    /// `docs/signal-collection.md`.
    pub monitor_agents: Vec<String>,
    /// Glob patterns meaning "this pane is mid-build; never reap it".
    pub busy_commands: Vec<String>,
    /// Substrings identifying agent helper processes (MCP servers, language
    /// servers, parsers).
    pub orphan_patterns: Vec<String>,
    /// A session or pane idle at least this many days is stale.
    pub stale_days: u64,
}

impl Default for CensusConfig {
    fn default() -> Self {
        Self {
            agent_clis: strs(&[
                "claude",
                "kiro-cli",
                "codex",
                "opencode",
                "gemini",
                "cursor-agent",
            ]),
            app_groups: strs(&[
                "Google Chrome",
                "Slack",
                "Code Helper",
                "Electron",
                "Docker",
                "zoom.us",
                "Ghostty",
            ]),
            // Two generic placeholders, matching the posture of the template's
            // own committed default. A user with managed agents adds the real
            // names in the overlay.
            monitor_agents: strs(&["sysmond", "mdworker"]),
            busy_commands: strs(&[
                "cargo",
                "make",
                "cmake",
                "rustc",
                "npm",
                "pnpm",
                "yarn",
                "node",
                "tsc",
                "jest",
                "vitest",
                "python*",
                "pytest",
                "go",
                "swift*",
                "gradle*",
                "xcodebuild",
                "bazel",
            ]),
            // `perf-scan` uses the regex `mcp|language.server|tree.sitter`. These
            // are literal alternatives with `.` standing in for any separator, so
            // a case-insensitive substring list covers it without pulling in a
            // regex dependency. The separator variants are spelled out rather
            // than matched loosely, because normalising separators away would
            // over-match unrelated binaries.
            orphan_patterns: strs(&[
                "mcp",
                "language-server",
                "language.server",
                "language server",
                "languageserver",
                "tree-sitter",
                "tree.sitter",
                "tree sitter",
                "treesitter",
            ]),
            stale_days: DEFAULT_STALE_DAYS,
        }
    }
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

/// The overlay file, as parsed. Every field optional; absent means "no change".
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct CensusOverlay {
    pub agent_clis: Option<Vec<String>>,
    pub agent_clis_add: Option<Vec<String>>,
    pub app_groups: Option<Vec<String>>,
    pub app_groups_add: Option<Vec<String>>,
    pub monitor_agents: Option<Vec<String>>,
    pub monitor_agents_add: Option<Vec<String>>,
    pub busy_commands: Option<Vec<String>>,
    pub busy_commands_add: Option<Vec<String>>,
    pub orphan_patterns: Option<Vec<String>>,
    pub orphan_patterns_add: Option<Vec<String>>,
    pub stale_days: Option<u64>,
}

impl CensusConfig {
    /// Every path an overlay may live at, in precedence order.
    ///
    /// **Two locations, deliberately.** `dirs::config_dir()` on macOS is
    /// `~/Library/Application Support` (NOT `~/.config` — an earlier version of
    /// this doc comment said otherwise and was simply wrong, which is how a live
    /// overlay went unread during the first end-to-end run). That is the
    /// platform convention the template mandates, so it wins. But `perf-scan`,
    /// whose configuration this replaces, reads `~/.config/perf-scan.local`, and
    /// a user who has kept a list there for a year should not have to discover a
    /// new directory to find out why their agents stopped being reported.
    ///
    /// Both are **outside the repository**, which is a stronger guarantee that
    /// site-specific process names cannot be committed than a `.gitignore`
    /// entry someone has to remember to add.
    pub fn overlay_paths() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(d) = dirs::config_dir() {
            out.push(d.join("banshee").join("census.local.toml"));
        }
        // The `perf-scan` habit. On macOS this is a genuinely different directory
        // from the one above; on Linux `dirs::config_dir()` already IS ~/.config,
        // so guard against listing it twice.
        if let Some(h) = dirs::home_dir() {
            let dot = h.join(".config").join("banshee").join("census.local.toml");
            if !out.contains(&dot) {
                out.push(dot);
            }
        }
        out
    }

    /// The first overlay path that exists, if any.
    pub fn overlay_path() -> Option<PathBuf> {
        Self::overlay_paths().into_iter().find(|p| p.exists())
    }

    /// The path an overlay SHOULD be written to (the platform-conventional one),
    /// for docs and error messages, whether or not it exists yet.
    pub fn preferred_overlay_path() -> Option<PathBuf> {
        Self::overlay_paths().into_iter().next()
    }

    /// Defaults, plus the first overlay found.
    pub fn load() -> Result<Self> {
        match Self::overlay_path() {
            Some(p) => Self::load_from(&p),
            None => Ok(Self::default()),
        }
    }

    /// Defaults, plus a specific overlay file.
    ///
    /// A malformed overlay is an ERROR, not a silent fall back to defaults.
    /// Quietly ignoring it would leave the census matching a generic list while
    /// the user believed their agents were configured — and the symptom would be
    /// "the monitoring-agent section is empty", which reads as good news.
    pub fn load_from(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let overlay: CensusOverlay = toml::from_str(&raw).map_err(|e| {
            CoreError::InvalidInput(format!(
                "cannot parse census overlay {}: {e}",
                path.display()
            ))
        })?;
        Ok(Self::default().with_overlay(overlay))
    }

    pub fn with_overlay(mut self, o: CensusOverlay) -> Self {
        apply(&mut self.agent_clis, o.agent_clis, o.agent_clis_add);
        apply(&mut self.app_groups, o.app_groups, o.app_groups_add);
        apply(
            &mut self.monitor_agents,
            o.monitor_agents,
            o.monitor_agents_add,
        );
        apply(
            &mut self.busy_commands,
            o.busy_commands,
            o.busy_commands_add,
        );
        apply(
            &mut self.orphan_patterns,
            o.orphan_patterns,
            o.orphan_patterns_add,
        );
        if let Some(d) = o.stale_days {
            self.stale_days = d;
        }
        self
    }

    // ---- matching ------------------------------------------------------

    /// Exact program match, `pgrep -x` semantics.
    pub fn is_agent_cli(&self, program: &str) -> bool {
        self.agent_clis.iter().any(|c| c == program)
    }

    /// Case-insensitive substring match against the command line.
    pub fn is_orphan_candidate(&self, args: &str) -> bool {
        let hay = args.to_lowercase();
        self.orphan_patterns
            .iter()
            .any(|p| hay.contains(&p.to_lowercase()))
    }

    /// Every configured app group whose name appears in the command line.
    /// Case-SENSITIVE, matching `grep -F`.
    pub fn app_groups_matching<'a>(&'a self, args: &str) -> impl Iterator<Item = &'a str> {
        let args = args.to_string();
        self.app_groups
            .iter()
            .filter(move |g| args.contains(g.as_str()))
            .map(|g| g.as_str())
    }

    /// Every configured monitoring agent whose name appears in the command line.
    /// Case-INSENSITIVE, matching `grep -iF`.
    pub fn monitor_agents_matching<'a>(&'a self, args: &str) -> impl Iterator<Item = &'a str> {
        let hay = args.to_lowercase();
        self.monitor_agents
            .iter()
            .filter(move |a| hay.contains(&a.to_lowercase()))
            .map(|a| a.as_str())
    }

    /// True when a tmux pane's command means "mid-build, do not reap".
    ///
    /// The command is lowercased by the caller (`TmuxPane::command_for_matching`)
    /// because tmux capitalizes framework interpreters.
    pub fn is_busy_command(&self, lowercased_command: &str) -> bool {
        self.busy_commands
            .iter()
            .any(|pat| glob_match(&pat.to_lowercase(), lowercased_command))
    }
}

fn apply(target: &mut Vec<String>, replace: Option<Vec<String>>, add: Option<Vec<String>>) {
    if let Some(r) = replace {
        *target = r;
    }
    if let Some(a) = add {
        target.extend(a);
    }
}

/// Shell-style glob with `*` wildcards, which is all `perf-scan`'s
/// `BUSY_COMMANDS` patterns use (`python*`, `swift*`, `gradle*`, `cargo*`).
///
/// Implemented directly rather than pulling a glob crate: the patterns are
/// matched against a single short command name, and `case "$c" in $pat)` in shell
/// is exactly this. `*` matches any run of characters including none.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // Split on '*' and walk the literal segments in order. A leading empty
    // segment means the pattern started with '*' (no anchor at the front); a
    // trailing empty segment means it ended with '*' (no anchor at the end).
    let segments: Vec<&str> = pattern.split('*').collect();
    if segments.len() == 1 {
        return pattern == text;
    }

    let mut pos = 0usize;
    for (i, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            continue;
        }
        if i == 0 {
            // Must match at the very start.
            if !text[pos..].starts_with(seg) {
                return false;
            }
            pos += seg.len();
        } else if i == segments.len() - 1 {
            // Must match at the very end, at or after the current position.
            return text[pos..].ends_with(seg) && text.len() - pos >= seg.len();
        } else {
            match text[pos..].find(seg) {
                Some(idx) => pos += idx + seg.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- defaults --------------------------------------------------------

    /// The committed defaults must never carry site- or vendor-specific
    /// security-agent product names — those belong only in the machine-local
    /// overlay. Asserting an exact ALLOWLIST (not a denylist of the real product
    /// names, which would itself leak them into a public repo) means any real
    /// name added to `monitor_agents` fails the gate.
    #[test]
    fn committed_monitor_agents_defaults_are_generic_only() {
        let c = CensusConfig::default();
        assert_eq!(
            c.monitor_agents,
            strs(&["sysmond", "mdworker"]),
            "committed monitor_agents must stay generic Apple daemons; real \
             security-agent names belong only in the machine-local overlay"
        );
    }

    #[test]
    fn defaults_cover_the_agent_clis_perf_scan_watched() {
        let c = CensusConfig::default();
        for cli in [
            "claude",
            "kiro-cli",
            "codex",
            "opencode",
            "gemini",
            "cursor-agent",
        ] {
            assert!(c.is_agent_cli(cli), "{cli} must be a known agent CLI");
        }
        assert_eq!(c.stale_days, 2);
    }

    // ---- overlay semantics ----------------------------------------------

    /// `monitor_agents` REPLACES, because a user's real agent list has nothing to
    /// do with our generic placeholders. Mutation-proof: make it append and the
    /// placeholders survive alongside the real names.
    #[test]
    fn a_replace_key_discards_the_defaults() {
        let toml = r#"monitor_agents = ["MyEDR", "MyDLP"]"#;
        let o: CensusOverlay = toml::from_str(toml).unwrap();
        let c = CensusConfig::default().with_overlay(o);
        assert_eq!(c.monitor_agents, vec!["MyEDR", "MyDLP"]);
        assert!(
            !c.monitor_agents.iter().any(|a| a == "sysmond"),
            "replace must discard the default placeholders"
        );
    }

    /// `app_groups_add` APPENDS, because the generic app list is genuinely
    /// useful and a user only wants to extend it. Mutation-proof: make it replace
    /// and Chrome stops being tracked.
    #[test]
    fn an_add_key_extends_the_defaults() {
        let toml = r#"app_groups_add = ["Example Suite"]"#;
        let o: CensusOverlay = toml::from_str(toml).unwrap();
        let c = CensusConfig::default().with_overlay(o);
        assert!(c.app_groups.iter().any(|g| g == "Google Chrome"));
        assert!(c.app_groups.iter().any(|g| g == "Example Suite"));
    }

    /// Both forms on one list: replace first, then append to the replacement.
    #[test]
    fn replace_and_add_compose_in_that_order() {
        let toml = r#"
            busy_commands = ["cargo"]
            busy_commands_add = ["mybuild*"]
        "#;
        let o: CensusOverlay = toml::from_str(toml).unwrap();
        let c = CensusConfig::default().with_overlay(o);
        assert_eq!(c.busy_commands, vec!["cargo", "mybuild*"]);
    }

    #[test]
    fn an_empty_overlay_changes_nothing() {
        let o: CensusOverlay = toml::from_str("").unwrap();
        assert_eq!(
            CensusConfig::default().with_overlay(o),
            CensusConfig::default()
        );
    }

    #[test]
    fn stale_days_is_configurable() {
        let o: CensusOverlay = toml::from_str("stale_days = 7").unwrap();
        assert_eq!(CensusConfig::default().with_overlay(o).stale_days, 7);
    }

    /// A typo'd key must be a loud error, not a silent no-op. Somebody who
    /// writes `monitor_agent` (singular) and sees no complaint will believe their
    /// agents are configured while the census matches a generic list — and the
    /// symptom is an EMPTY monitoring section, which reads as good news.
    /// Mutation-proof: drop `deny_unknown_fields` and this fails.
    #[test]
    fn an_unknown_overlay_key_is_rejected() {
        let err = toml::from_str::<CensusOverlay>(r#"monitor_agent = ["x"]"#).unwrap_err();
        assert!(
            err.to_string().contains("monitor_agent"),
            "error should name the bad key: {err}"
        );
    }

    #[test]
    fn a_malformed_overlay_file_is_an_error_not_a_silent_default() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("census.local.toml");
        std::fs::write(&p, "monitor_agents = [unclosed").unwrap();
        let err = CensusConfig::load_from(&p).unwrap_err();
        assert!(err.to_string().contains("cannot parse census overlay"));
    }

    #[test]
    fn a_valid_overlay_file_loads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("census.local.toml");
        std::fs::write(
            &p,
            "monitor_agents = [\"ExampleEDR\"]\napp_groups_add = [\"ExampleApp\"]\nstale_days = 3\n",
        )
        .unwrap();
        let c = CensusConfig::load_from(&p).unwrap();
        assert_eq!(c.monitor_agents, vec!["ExampleEDR"]);
        assert!(c.app_groups.iter().any(|g| g == "ExampleApp"));
        assert_eq!(c.stale_days, 3);
    }

    /// Every candidate overlay path lives OUTSIDE the repository — a stronger
    /// guarantee than a `.gitignore` entry someone has to remember to add.
    #[test]
    fn every_overlay_path_is_outside_the_repository() {
        let paths = CensusConfig::overlay_paths();
        assert!(!paths.is_empty(), "at least one candidate path");
        for p in &paths {
            let s = p.to_string_lossy();
            assert!(s.ends_with("banshee/census.local.toml"), "got {s}");
            assert!(
                !s.contains("/Code/banshee/"),
                "must not be inside the repo: {s}"
            );
        }
    }

    /// On macOS the platform config dir is `~/Library/Application Support`, and
    /// the `perf-scan` habit of `~/.config` is a DIFFERENT directory. Both must
    /// be searched: the first version of this code checked only the platform one,
    /// and a real overlay sat unread through an end-to-end run while the log
    /// cheerfully reported `present=false`.
    #[test]
    #[cfg(target_os = "macos")]
    fn both_the_platform_dir_and_the_dotconfig_habit_are_searched() {
        let paths = CensusConfig::overlay_paths();
        assert_eq!(paths.len(), 2, "got {paths:?}");
        let joined: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into()).collect();
        assert!(
            joined[0].contains("Library/Application Support"),
            "the platform dir must win: {joined:?}"
        );
        assert!(
            joined[1].contains("/.config/"),
            "the perf-scan habit must be a fallback: {joined:?}"
        );
    }

    /// No duplicate candidates, so a Linux build (where `config_dir()` already
    /// IS `~/.config`) does not read the same file twice.
    #[test]
    fn overlay_paths_are_distinct() {
        let paths = CensusConfig::overlay_paths();
        let mut sorted = paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), paths.len(), "duplicate candidate: {paths:?}");
    }

    /// `load()` with no overlay anywhere returns the generic defaults rather
    /// than erroring — a fresh machine has no overlay and that is fine.
    #[test]
    fn load_without_an_overlay_returns_the_defaults() {
        // Only assert the shape when this machine genuinely has no overlay;
        // otherwise the assertion would depend on the developer's own config.
        if CensusConfig::overlay_path().is_none() {
            assert_eq!(CensusConfig::load().unwrap(), CensusConfig::default());
        }
    }

    // ---- matching --------------------------------------------------------

    /// Agent CLI matching is EXACT. A substring test would match `claude-fable`,
    /// `claudia`, or a wrapper script.
    #[test]
    fn agent_cli_matching_is_exact_not_a_prefix() {
        let c = CensusConfig::default();
        assert!(c.is_agent_cli("claude"));
        assert!(!c.is_agent_cli("claude-wrapper"));
        assert!(!c.is_agent_cli("myclaude"));
        assert!(!c.is_agent_cli("clau"));
    }

    #[test]
    fn orphan_matching_is_case_insensitive_and_covers_separator_variants() {
        let c = CensusConfig::default();
        for args in [
            "/x/example-mcp-server",
            "/x/Example-MCP-Server",
            "/x/rust-language-server",
            "/x/rust language server",
            "/x/tree-sitter-cli",
            "/x/tree.sitter",
        ] {
            assert!(c.is_orphan_candidate(args), "{args} should match");
        }
        for args in ["/usr/bin/zsh", "/x/postgres", "/x/serverless"] {
            assert!(!c.is_orphan_candidate(args), "{args} should NOT match");
        }
    }

    /// App-group matching is case-SENSITIVE, matching `grep -F`. "Slack" must not
    /// be found by way of "slackware".
    #[test]
    fn app_group_matching_is_case_sensitive_substring() {
        let c = CensusConfig::default();
        let hits: Vec<&str> = c
            .app_groups_matching("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .collect();
        assert_eq!(hits, vec!["Google Chrome"]);

        let none: Vec<&str> = c.app_groups_matching("/usr/bin/google chrome").collect();
        assert!(none.is_empty(), "lowercase must not match: {none:?}");
    }

    #[test]
    fn monitor_agent_matching_is_case_insensitive() {
        let c = CensusConfig::default()
            .with_overlay(toml::from_str(r#"monitor_agents = ["ExampleEDR"]"#).unwrap());
        let hits: Vec<&str> = c
            .monitor_agents_matching("/Library/Extensions/exampleedr.systemextension/x")
            .collect();
        assert_eq!(hits, vec!["ExampleEDR"], "must match despite the casing");
    }

    // ---- glob ------------------------------------------------------------

    #[test]
    fn glob_matches_the_busy_command_patterns_perf_scan_uses() {
        assert!(glob_match("cargo", "cargo"));
        assert!(!glob_match("cargo", "cargo-sweep"));
        assert!(glob_match("python*", "python"));
        assert!(glob_match("python*", "python3"));
        assert!(glob_match("python*", "python3.12"));
        assert!(!glob_match("python*", "mypython"));
        assert!(glob_match("swift*", "swift-frontend"));
        assert!(glob_match("gradle*", "gradlew"));
    }

    #[test]
    fn glob_handles_leading_and_middle_wildcards() {
        assert!(glob_match("*build", "xcodebuild"));
        assert!(glob_match("*build", "build"));
        assert!(!glob_match("*build", "builder"));
        assert!(glob_match("a*c", "abc"));
        assert!(glob_match("a*c", "ac"));
        assert!(!glob_match("a*c", "abd"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
    }

    /// The whole point of the busy list: a pane mid-build must never be reaped.
    /// tmux reports framework interpreters capitalized, so matching happens on
    /// the lowercased command. Mutation-proof: compare the raw pattern against
    /// the raw command and `Python` stops matching `python*`.
    #[test]
    fn a_capitalized_interpreter_still_counts_as_busy() {
        let c = CensusConfig::default();
        // What TmuxPane::command_for_matching hands us.
        assert!(c.is_busy_command("python"), "python must be busy");
        assert!(c.is_busy_command(&"Python".to_lowercase()));
        assert!(c.is_busy_command("cargo"));
        assert!(!c.is_busy_command("zsh"), "an idle shell is not busy");
        assert!(!c.is_busy_command("bash"));
    }
}
