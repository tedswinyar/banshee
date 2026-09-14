// Key-file auth: a random key generated on first run, stored 0600, sent by
// clients as `X-Api-Key`. This is a local-loopback trust boundary — it keeps
// other local users and stray browser JS out, nothing more. /health is
// unauthenticated so process supervisors can probe without the key.

use std::io::Write;
use std::path::Path;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use crate::AppState;

pub const API_KEY_HEADER: &str = "x-api-key";

/// Why an existing key file could not be trusted, and therefore had to be replaced.
///
/// Kept as a type rather than a bool so the log line can say WHICH property failed —
/// "the key was world-readable" and "the key was empty" call for very different
/// human reactions, and a monitor that blurs them teaches people to ignore it.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDistrust {
    /// Group or other had any bit set. The key must be assumed READ by them.
    Permissive(u32),
    /// Present but blank — a stub, a truncated restore, a failed write.
    Blank,
}

#[cfg(unix)]
impl KeyDistrust {
    fn reason(self) -> String {
        match self {
            KeyDistrust::Permissive(mode) => {
                format!("mode {:04o} allowed group/other access", mode & 0o7777)
            }
            KeyDistrust::Blank => "file was present but empty".to_string(),
        }
    }
}

/// Load the API key from `path`, generating one (0600) if absent — and ROTATING it
/// rather than repairing it if the existing file could not be trusted.
///
/// Rotation, not `chmod`, is the load-bearing decision here (`banshee-4d0`). An
/// earlier version detected a group/other-readable key and fixed the mode, which
/// left the daemon serving *the key that had just been readable by every local
/// user*. Tightening the permissions on a secret that has already been exposed
/// restores the appearance of the boundary and not the boundary: the only sound
/// response to "this may have leaked" is a new secret. Replacing the file also
/// sidesteps in-place ACL repair — `chmod 600` does NOT remove a macOS ACL, so a
/// mode that reads as 0600 can still be world-readable through an ACL entry, and
/// `ls -l` cannot show you that it is.
pub fn load_or_create_key(path: &Path) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        match open_existing_key_nofollow(path)? {
            Some((key, None)) => Ok(key),
            Some((_, Some(distrust))) => {
                // The old key is deliberately NOT returned, not even once.
                tracing::warn!(
                    path = %path.display(),
                    reason = %distrust.reason(),
                    "API key could not be trusted; generating a new one. \
                     Clients using the old key must re-read the key file."
                );
                write_new_key(path)
            }
            None => write_new_key(path),
        }
    }
    #[cfg(not(unix))]
    {
        if let Ok(existing) = std::fs::read_to_string(path) {
            let key = existing.trim().to_string();
            if !key.is_empty() {
                return Ok(key);
            }
        }
        write_new_key(path)
    }
}

/// Open an existing key file WITHOUT following symlinks and read it through that
/// same descriptor, returning the key plus why it should be distrusted (if it
/// should). `None` means there is no usable file yet.
///
/// Every check runs against the OPEN DESCRIPTOR (`fstat`), never against the path.
/// The previous implementation called `symlink_metadata(path)` and then
/// `read_to_string(path)` — two independent path lookups, so an attacker who could
/// write the containing directory had a window between them to swap a validated
/// regular file for a symlink pointing anywhere. `O_NOFOLLOW` makes the refusal
/// atomic (the open itself fails on a symlink), and checking the fd means the file
/// that was validated is provably the file that was read.
#[cfg(unix)]
fn open_existing_key_nofollow(
    path: &Path,
) -> std::io::Result<Option<(String, Option<KeyDistrust>)>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).custom_flags(libc::O_NOFOLLOW);
    let mut file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // ELOOP is what O_NOFOLLOW reports for a symlink. Refuse loudly rather than
        // creating a new key, because writing would follow the link and could plant
        // our secret at the attacker's chosen path.
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "API key file is a symlink; refusing to read or replace it",
            ));
        }
        Err(e) => return Err(e),
    };

    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "API key file must be a regular file, not a device or directory",
        ));
    }
    // Wrong owner is an ERROR, never a rotation. Replacing a file we do not own
    // would be us clobbering another user's data on their behalf, and it is the
    // signature of a planted file — fail closed and let a human look.
    let euid = unsafe { libc::geteuid() };
    if meta.uid() != euid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "API key file is owned by uid {} but this process runs as uid {euid}; \
                 refusing to trust or replace it",
                meta.uid()
            ),
        ));
    }

    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    let key = contents.trim().to_string();

    let mode = meta.mode();
    let distrust = if mode & 0o077 != 0 {
        Some(KeyDistrust::Permissive(mode))
    } else if key.is_empty() {
        Some(KeyDistrust::Blank)
    } else {
        None
    };
    Ok(Some((key, distrust)))
}

/// Write a freshly generated key, atomically, at 0600.
///
/// Written to a temporary file in the same directory and `rename`d into place, so a
/// reader never observes a half-written key and so the mode is 0600 from the instant
/// the file exists — `OpenOptions::mode` applies only on CREATION, and an earlier
/// version that opened a pre-existing 0644 stub kept the old mode while writing a
/// brand-new secret into it. `rename` over a symlink replaces the LINK, which is why
/// rotation cannot be used to write through one.
fn write_new_key(path: &Path) -> std::io::Result<String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key = Uuid::new_v4().to_string();
    let tmp = path.with_extension(format!("incoming.{}", std::process::id()));

    let mut opts = std::fs::OpenOptions::new();
    // O_EXCL: never write into a file somebody else placed at our temp path.
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = match opts.open(&tmp) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // A stale temp from a crashed run. Remove and retry once; still
            // O_EXCL, so a racing writer loses rather than being written through.
            std::fs::remove_file(&tmp)?;
            opts.open(&tmp)?
        }
        Err(e) => return Err(e),
    };
    writeln!(f, "{key}")?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(key)
}

/// The body every keyed route returns over TCP, key or no key (ADR-0008 Phase 3).
///
/// Exported so the integration test can assert on the SAME string rather than a copy
/// that drifts, and so a client that wants to recognise the case can match it.
pub const TCP_REFUSAL: &str = "credentials are not accepted over TCP; every authenticated \
     route is served on the daemon's Unix socket (ADR-0008). This client is either \
     older than the daemon or was pointed at a URL — update it, or unset BANSHEE_API_URL";

/// Refuse every request on a keyed route that arrived over TCP — before, and instead
/// of, looking at any key it carried.
///
/// Until Phase 3 the daemon still honoured a valid `X-Api-Key` on the loopback port,
/// which is what kept every pre-ADR-0008 client working: they sent the file key to a
/// squattable port and the daemon said yes. Refusing here is what makes those clients
/// VISIBLY stale so they get updated, and what guarantees nothing new comes to depend
/// on the port. The status is 401 on purpose: every client already has a 401 path that
/// says "the daemon is running and this client is locked out", and that is the truth.
/// A 404 would read as a missing route and a connection refusal as a dead daemon.
///
/// It logs once per process, at warn, and never again — a stale menu-bar app polls
/// every few seconds and a warning per poll would bury everything else in the log.
pub async fn refuse_over_tcp(request: Request, _next: Next) -> Response {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            path = %request.uri().path(),
            keyed = request.headers().contains_key(API_KEY_HEADER),
            "refused a request on a keyed route over TCP; a client older than ADR-0008 \
             Phase 3 (or one pointed at BANSHEE_API_URL) is still using the port. \
             Logged once per process."
        );
    }
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({ "error": TCP_REFUSAL })),
    )
        .into_response()
}

pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok());
    // Constant-time compare so key verification cannot leak, via response
    // timing, how many leading bytes were correct. `constant_time_eq` returns
    // false immediately on a length mismatch (length is not secret here) and
    // otherwise compares every byte regardless of where they differ. `&str`
    // `!=` would short-circuit on the first differing byte — a timing side
    // channel. Loopback noise dwarfs the signal today, but this is the
    // template's auth primitive: it should model the right habit.
    let authorized = match presented {
        Some(p) => constant_time_eq::constant_time_eq(p.as_bytes(), state.api_key.as_bytes()),
        None => false,
    };
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "error": "missing or invalid X-Api-Key"
            })),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_key_once_and_reuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        let first = load_or_create_key(&path).unwrap();
        let second = load_or_create_key(&path).unwrap();
        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        load_or_create_key(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn blank_key_file_is_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        std::fs::write(&path, "\n").unwrap();
        let key = load_or_create_key(&path).unwrap();
        assert!(!key.is_empty());
    }

    // A pre-existing BLANK file is opened (not created), so `opts.mode(0o600)`
    // does not apply — the regenerated key must still end up 0600, not inherit a
    // loose stub mode. Pins the blank-file permission gap.
    #[cfg(unix)]
    #[test]
    fn regenerated_key_over_a_blank_stub_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        std::fs::write(&path, "\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let key = load_or_create_key(&path).unwrap();
        assert!(!key.is_empty());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "regenerated key must not be group/other-readable"
        );
    }

    /// A key that was group/other-readable must be ROTATED, not repaired.
    ///
    /// This test previously asserted the OPPOSITE — that the existing key is kept and
    /// its mode fixed — and it passed, which is why the weakness survived having a
    /// test at all. Tightening permissions on a secret that every local user could
    /// already read restores the appearance of the trust boundary, not the boundary.
    /// The old key must not come back, not even once.
    ///
    /// Mutation-proof: return the loaded key instead of calling `write_new_key` on
    /// `Some(distrust)` and this fails on the first assertion.
    #[cfg(unix)]
    #[test]
    fn an_overpermissive_key_is_rotated_not_merely_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        std::fs::write(&path, "deadbeef\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let key = load_or_create_key(&path).unwrap();
        assert_ne!(
            key, "deadbeef",
            "a key that was world-readable must be replaced, not re-served"
        );
        assert!(!key.is_empty());
        // The file on disk must hold the NEW key, so the next start agrees.
        let on_disk = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert_eq!(on_disk, key, "the rotated key must be persisted");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the rotated key must not be group/other-readable"
        );
    }

    /// Owner-executable-only bits are not a leak, so a 0700 key is NOT rotated.
    ///
    /// The pair matters: without this, "rotate whenever the mode is not exactly 0600"
    /// would pass the test above while throwing the key away on every start for a
    /// harmless mode, and a key that changes every start is an outage rather than a
    /// security control. This is the case that makes the mask `0o077` load-bearing
    /// rather than an equality check against 0o600.
    #[cfg(unix)]
    #[test]
    fn an_owner_only_key_with_extra_owner_bits_is_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        std::fs::write(&path, "deadbeef\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let key = load_or_create_key(&path).unwrap();
        assert_eq!(
            key, "deadbeef",
            "0700 exposes nothing to group/other; rotating would be a self-inflicted outage"
        );
    }

    /// The read must come from the descriptor that was VALIDATED, not from a second
    /// path lookup. Proven the only way a unit test can: hand the loader a path whose
    /// final component is a symlink and require that the open itself fails, rather
    /// than the old shape where `symlink_metadata` said "regular file" and a separate
    /// `read_to_string` re-resolved the path.
    ///
    /// A symlink must also never be REPLACED — rotating through one would plant the
    /// new key at the attacker's chosen target.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_key_file_is_neither_read_nor_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real_key");
        std::fs::write(&real, "deadbeef\n").unwrap();
        let link = dir.path().join("api_key");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let err = load_or_create_key(&link).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        // The target must be untouched: not read into service, not overwritten.
        assert_eq!(
            std::fs::read_to_string(&real).unwrap().trim(),
            "deadbeef",
            "the symlink target must not be rewritten"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink itself must be left in place for a human to inspect"
        );
    }

    /// A fresh key is created 0600 from the instant it exists, via a temp file and a
    /// rename rather than an in-place write. Pins that the atomic path is used.
    #[cfg(unix)]
    #[test]
    fn a_rotation_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api_key");
        let key = load_or_create_key(&path).unwrap();
        assert!(!key.is_empty());
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("incoming"))
            .collect();
        assert!(strays.is_empty(), "left temp files behind: {strays:?}");
    }

    // The constant-time compare must still be a CORRECT equality: exact match
    // accepts, and every mismatch class (wrong byte, prefix, different length)
    // rejects. Timing cannot be asserted in a unit test, but this pins that a
    // "simplification" back to prefix/loose matching would be caught.
    #[test]
    fn key_comparison_is_exact_equality() {
        use constant_time_eq::constant_time_eq;
        let key = "e7ae86e2-308b-444c-8a3d-cd21467ab442";
        assert!(constant_time_eq(key.as_bytes(), key.as_bytes()));
        // One byte off.
        assert!(!constant_time_eq(
            key.as_bytes(),
            "e7ae86e2-308b-444c-8a3d-cd21467ab443".as_bytes()
        ));
        // A correct prefix is not a match.
        assert!(!constant_time_eq(key.as_bytes(), "e7ae86e2".as_bytes()));
        // Empty presented key never matches a real one.
        assert!(!constant_time_eq(key.as_bytes(), b""));
    }
}
