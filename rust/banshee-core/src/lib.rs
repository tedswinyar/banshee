// banshee-core owns the domain model, its SQLite persistence, and the pressure
// reduction. Nothing in this crate knows about HTTP, MCP, or the CLI.
//
// The template's `Note` demonstration slice lived here until schema v6;
// the pressure domain replaced it. What survived is the SHAPE it demonstrated:
// camelCase wire types with explicit nulls, a store that owns its schema, and
// fixtures shared with the Swift side.

pub mod actions;
pub mod backup;
pub mod census;
pub mod collector;
pub mod pressure;
pub mod rates;
pub mod sample;
pub mod sample_store;
pub mod schema;
pub mod sys;
// Blocking HTTP over the Unix-domain socket, shared by the CLI and the MCP server
// (ADR-0008). One implementation, because two hand-rolled ones would drift — in the
// transport that carries the API key.
pub mod uds_http;
pub mod wire_time;

pub use collector::Collector;
pub use pressure::headroom::Headroom;
pub use pressure::{Pressure, evaluate};
pub use sample::{Rollup, Sample, VolumeSample};
pub use sample_store::{Retention, SampleStore, SweepReport};
pub use sys::{RealProbe, SysProbe, SysReading, VolumeReading};

use thiserror::Error;

/// The port the API listens on in the `prod` profile, and the port every client
/// falls back to when `BANSHEE_API_URL` is unset.
///
/// **One literal, in core, on purpose.** Another project shipped a server
/// defaulting to one port while its config crate defaulted to another, because
/// two places independently knew "the port" — the same class of bug ADR-0005 keeps the
/// severity thresholds out of. This number once appeared in four places
/// (the API's config, the CLI, the MCP server, and Swift's `APIClient`); the
/// three Rust ones now read it from here, and Swift's copy is pinned against
/// this file by `APIClientTests.testDefaultPortMatchesTheRustConstant`.
pub const DEFAULT_API_PORT: u16 = 18769;

/// The `dev` profile's port. Deliberately a different literal, ten above prod,
/// so a dev daemon and a prod daemon can run at once.
pub const DEV_API_PORT: u16 = 18779;

/// The loopback base URL for a port. Clients build their default from this so
/// none of them can spell the host or scheme differently.
pub fn api_url_for_port(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// The default base URL every client uses when the environment says nothing.
pub fn default_api_url() -> String {
    api_url_for_port(DEFAULT_API_PORT)
}

/// The env var every client reads to find the Unix-domain socket (ADR-0008).
///
/// Deliberately SEPARATE from `BANSHEE_API_URL` rather than encoding the path into
/// it (`http+unix://%2FUsers%2F…`): percent-encoding a filesystem path is a reliable
/// footgun, and `BANSHEE_API_URL` is what [`host_is_loopback`] parses to decide
/// whether the key may be attached. Overloading that string would invite exactly the
/// confusion the gate exists to prevent.
pub const API_SOCKET_ENV: &str = "BANSHEE_API_SOCKET";

/// The socket file's name inside the app's config directory.
pub const API_SOCKET_FILE: &str = "api.sock";

/// **`sun_path` is 104 bytes on macOS**, including the NUL — a hard kernel limit,
/// not a guideline, and `bind` fails with `EINVAL` past it rather than truncating
/// visibly. The prod path is ~60 bytes; a socket under `TMPDIR` in a test is ~79,
/// which fits but leaves little room, so a test that nests a few directories deep
/// can fail for a reason that looks nothing like "path too long".
pub const MAX_SOCKET_PATH_LEN: usize = 104;

/// Whether `path` fits in `sockaddr_un`. Checked before `bind` and before `connect`
/// so the error names the real problem.
pub fn socket_path_fits(path: &std::path::Path) -> bool {
    path.as_os_str().len() < MAX_SOCKET_PATH_LEN
}

/// True if the URL's host is loopback (127.0.0.0/8, ::1, or "localhost").
/// Clients auto-attach the file-loaded API key ONLY to a loopback host — that key
/// authorizes the reap/kill routes and must never be sent to an arbitrary
/// `BANSHEE_API_URL` (banshee-4d0). An explicit `BANSHEE_API_KEY` is the user's own
/// choice and is not gated by this.
pub fn host_is_loopback(url: &str) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};
    let authority = url.split("://").nth(1).unwrap_or(url);
    let authority = authority.split('/').next().unwrap_or("");
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<Ipv4Addr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
        || host
            .parse::<Ipv6Addr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Cadence of the cheap tier, in seconds.
///
/// **Lives here because TWO things need it**: the sampler, which ticks at it, and
/// the pressure model, which has to know what a normal interval looks like in order
/// to recognise a GAP (`banshee-s6s`). Before this constant existed the sampler
/// named 15 and the model's `window_samples` comment merely mentioned it, so the
/// model could not tell a missed tick from a laptop that had been asleep for an
/// hour — and a gap read as continuous evidence, which turned the first red reading
/// after any restart into a "sustained" one.
///
/// `SamplerConfig::from_env` propagates the ACTUAL resolved interval into
/// `PressureConfig`, so an overridden cadence stays consistent with the model.
pub const DEFAULT_SAMPLE_INTERVAL_SECS: u64 = 15;

/// Cadence of the census tier, in seconds. Twenty times the cheap tier, because it
/// spawns subprocesses and the cheap tier deliberately does not (ADR-0007).
pub const DEFAULT_CENSUS_INTERVAL_SECS: u64 = 300;

/// The census tier must stay MUCH slower than the cheap tier. The gap detector
/// relies on the two being distinguishable: an ordinary census interval must never
/// look like a gap in the sample series, and a hole the size of a census must never
/// look like ordinary sampling.
///
/// A compile-time assertion rather than a test, because it is a statement about two
/// constants — it cannot be skipped, and it fails the build instead of a suite.
const _: () = assert!(DEFAULT_CENSUS_INTERVAL_SECS >= DEFAULT_SAMPLE_INTERVAL_SECS * 10);

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("schema error: {0}")]
    Schema(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;

#[cfg(test)]
mod tests {
    use super::*;

    /// The port constants are the single source of truth, and dev must not
    /// collide with prod. Mutation-proof: set `DEV_API_PORT` to
    /// `DEFAULT_API_PORT` and this fails — which is the state where starting a
    /// dev daemon silently steals the prod port (or climbs the ladder into it).
    #[test]
    fn the_dev_port_cannot_collide_with_prod() {
        assert_ne!(DEFAULT_API_PORT, DEV_API_PORT);
        // The API climbs a ten-step port ladder when its port is busy, so the
        // two profiles need more than one port between them.
        assert!(
            DEV_API_PORT.abs_diff(DEFAULT_API_PORT) >= 10,
            "the port ladder is 10 steps wide; {DEFAULT_API_PORT} and \
             {DEV_API_PORT} are too close"
        );
    }

    /// Every client builds its default URL from one function, so host and
    /// scheme cannot drift between the CLI, the MCP server and Swift.
    #[test]
    fn the_default_url_is_loopback_and_carries_the_default_port() {
        assert_eq!(default_api_url(), "http://127.0.0.1:18769");
        assert_eq!(api_url_for_port(DEFAULT_API_PORT), default_api_url());
        assert!(
            default_api_url().starts_with("http://127.0.0.1:"),
            "the API is loopback-only (docs/threat-model.md)"
        );
    }

    /// The file-loaded production key must attach only to a loopback host.
    /// Mutation-proof: make `host_is_loopback` accept a public host and this fails
    /// — the state where a mis-set `BANSHEE_API_URL` exfiltrates the kill key.
    #[test]
    fn only_loopback_hosts_are_trusted_with_the_file_key() {
        for ok in [
            "http://127.0.0.1:18769",
            "http://localhost:9",
            "http://[::1]:5",
            "http://127.0.0.5",
        ] {
            assert!(host_is_loopback(ok), "{ok} should be loopback");
        }
        for bad in [
            "http://attacker.example",
            "http://192.0.2.1:18769",
            "https://banshee.example.com",
            "http://169.254.1.1",
        ] {
            assert!(!host_is_loopback(bad), "{bad} must NOT be loopback");
        }
    }
}
