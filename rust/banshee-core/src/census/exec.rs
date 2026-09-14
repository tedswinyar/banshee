// census/exec — the subprocess seam.
//
// The census tier is the ONE place Banshee spawns processes (ADR-0007), and this
// trait is where that happens, so the collector and every classification test
// runs against captured output rather than a live machine.

use std::process::Command;

use crate::{CoreError, Result};

/// Runs a command and returns its stdout.
pub trait CommandRunner: Send + Sync {
    /// `Err` means the command could not be SPAWNED (missing binary, no
    /// permission). A non-zero exit status is NOT an error: `lsof` exits 1 when
    /// the pid has gone, `tmux` exits 1 with no server running, and `ps` can
    /// exit non-zero while still printing useful rows. `perf-scan` swallows all
    /// of these with `2>/dev/null || true`, and the parsers here already treat
    /// unparseable output as "no data".
    fn run(&self, program: &str, args: &[&str]) -> Result<String>;
}

/// A shared runner is a runner. This is what lets a test hand an
/// `ActionRunner` a boxed `Arc<FakeRunner>` while keeping its own handle to
/// read the recorded calls back out.
impl<R: CommandRunner + ?Sized> CommandRunner for std::sync::Arc<R> {
    fn run(&self, program: &str, args: &[&str]) -> Result<String> {
        (**self).run(program, args)
    }
}

/// Real subprocesses.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| CoreError::Schema(format!("cannot run `{program}`: {e}")))?;
        // Lossy rather than strict UTF-8: a process can have arbitrary bytes in
        // its argv, and one such process must not blind the whole census.
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Replays canned stdout per command, and records what was asked for — which
    /// is how the "lsof only for flagged sessions" rule gets tested.
    pub struct FakeRunner {
        /// Successive responses per command; the LAST one repeats once the
        /// sequence is exhausted, so a single `.with()` behaves as before.
        responses: HashMap<String, Vec<String>>,
        cursors: Mutex<HashMap<String, usize>>,
        pub calls: Mutex<Vec<String>>,
        /// Commands whose spawn should fail, to exercise the degradation paths.
        pub unavailable: Vec<String>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self {
                responses: HashMap::new(),
                cursors: Mutex::new(HashMap::new()),
                calls: Mutex::new(Vec::new()),
                unavailable: Vec::new(),
            }
        }

        /// Key is `program` plus its args, space-joined.
        pub fn with(mut self, key: &str, stdout: &str) -> Self {
            self.responses
                .insert(key.to_string(), vec![stdout.to_string()]);
            self
        }

        /// Successive calls to `key` see successive entries; the last repeats.
        ///
        /// This is how "recompute at execute time" becomes testable at all: a
        /// runner that answers the same bytes forever cannot distinguish a
        /// function that re-reads the machine from one replaying a stale list.
        pub fn with_sequence(mut self, key: &str, outs: &[&str]) -> Self {
            self.responses.insert(
                key.to_string(),
                outs.iter().map(|s| (*s).to_string()).collect(),
            );
            self
        }

        pub fn unavailable(mut self, program: &str) -> Self {
            self.unavailable.push(program.to_string());
            self
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }

        pub fn calls_to(&self, program: &str) -> Vec<String> {
            self.calls()
                .into_iter()
                .filter(|c| c.starts_with(program))
                .collect()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> Result<String> {
            let key = if args.is_empty() {
                program.to_string()
            } else {
                format!("{program} {}", args.join(" "))
            };
            self.calls
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(key.clone());

            if self.unavailable.iter().any(|u| u == program) {
                return Err(CoreError::Schema(format!("cannot run `{program}`: fake")));
            }
            let Some(seq) = self.responses.get(&key) else {
                return Ok(String::new());
            };
            let mut cursors = self.cursors.lock().unwrap_or_else(|p| p.into_inner());
            let cursor = cursors.entry(key).or_insert(0);
            let out = seq.get(*cursor).or_else(|| seq.last()).cloned();
            *cursor += 1;
            Ok(out.unwrap_or_default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-zero exit must not be an error — `lsof` on a dead pid is the normal
    /// case, not a failure. Mutation-proof: add a `status.success()` check to
    /// `RealRunner::run` and this fails.
    #[test]
    fn a_non_zero_exit_is_not_an_error() {
        // `false` exits 1 with no output.
        let out = RealRunner
            .run("false", &[])
            .expect("non-zero exit is not an error");
        assert_eq!(out, "");
    }

    #[test]
    fn a_missing_binary_is_an_error() {
        let err = RealRunner
            .run("banshee-no-such-binary-exists", &[])
            .unwrap_err();
        assert!(err.to_string().contains("cannot run"));
    }

    #[test]
    fn captures_stdout() {
        let out = RealRunner.run("echo", &["hello"]).unwrap();
        assert_eq!(out.trim(), "hello");
    }

    /// Non-UTF-8 bytes in a command line must not blind the census.
    #[test]
    fn invalid_utf8_output_is_replaced_not_fatal() {
        let out = RealRunner
            .run("printf", &["a\\x80b"])
            .expect("must not fail on invalid utf-8");
        assert!(out.starts_with('a'), "got {out:?}");
    }
}
