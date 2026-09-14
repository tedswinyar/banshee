// Embed the git revision at build time, so the RUNNING daemon can say what it
// is. `/health` serves it (`gitRev`), which is the traceability the eight-day
// deleted-inode outage lacked: a process that cannot report its own revision
// leaves "is the latest code running?" unanswerable (the scar behind ADR-0004,
// sharpening review 2026-09-01).
//
// The DIRTY flag is best-effort: cargo reruns this script only when the paths
// below change, so an unstaged edit that rebuilds the crate without moving
// HEAD or the index can leave a stale `+dirty`/clean marker. That is accepted
// — the ENFORCEMENT against installing from a dirty tree lives in
// scripts/install-launchd.sh (which checks the tree at install time); this
// stamp is the audit trail, not the gate.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    // Rerun when HEAD moves (commit/checkout) or the index changes (stage).
    // Resolved via git itself so a worktree or a moved .git dir still works.
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/index");
    }

    // "unknown" rather than failing the build: a source tarball without .git
    // must still compile, and an unknown stamp is honest about itself.
    let rev = git(&["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    let dirty = if rev != "unknown" {
        // status --porcelain covers unstaged AND untracked-but-significant
        // states; diff-index alone misses new files.
        match git(&["status", "--porcelain"]) {
            Some(s) if !s.is_empty() => "+dirty",
            Some(_) => "",
            None => "",
        }
    } else {
        ""
    };
    println!("cargo:rustc-env=BANSHEE_GIT_REV={rev}{dirty}");
}
