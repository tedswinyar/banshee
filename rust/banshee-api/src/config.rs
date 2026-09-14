// Profiles: prod (default), dev, test — selected by BANSHEE_PROFILE.
//
//   profile  db path                                   port    key file
//   prod     <data_dir>/banshee/banshee.db       18769   <config_dir>/banshee/api_key
//   dev      <data_dir>/banshee/banshee-dev.db   18779   <config_dir>/banshee/api_key
//   test     BANSHEE_DB_PATH (required)             BANSHEE_PORT (required)
//                                                              BANSHEE_KEY_FILE (required)
//
// Test mode wins: it refuses to run against anything under the prod data
// directory, so a mis-set env var cannot touch real data. BANSHEE_PORT
// and BANSHEE_DB_PATH also override dev/prod when set explicitly.

use std::path::PathBuf;

// The port literals live in banshee-core, not here. This file once held
// 18769 and the CLI, the MCP server and Swift each held their own copy —
// four independent places that knew "the port", which is exactly how another
// project ended up serving on one port while its config crate said another.
use banshee_core::{DEFAULT_API_PORT, DEV_API_PORT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Prod,
    Dev,
    Test,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub profile: Profile,
    pub db_path: PathBuf,
    pub port: u16,
    pub key_file: PathBuf,
    /// The Unix-domain socket clients authenticate over (ADR-0008).
    ///
    /// Every profile has one, including `test` — the socket is the transport the
    /// security property depends on, so exercising it must not be optional in the
    /// suite that would catch a regression in it.
    pub socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("BANSHEE_PROFILE must be prod, dev, or test (got {0:?})")]
    BadProfile(String),
    #[error("test profile requires {0} to be set")]
    MissingTestEnv(&'static str),
    #[error("{0} is not a valid port: {1}")]
    BadPort(&'static str, String),
    #[error("test profile refuses a database under the prod data dir: {0}")]
    TestPointsAtProdData(String),
    #[error("cannot determine platform data/config directory")]
    NoPlatformDirs,
}

/// True if `db_path` lands inside `data_dir`, judged by both the literal
/// path (catches `..` traversal, which resolves away) and the canonicalized
/// deepest-existing ancestor (catches symlinks that point into the prod
/// dir). Either hit is a refusal — this is a data-safety backstop, so it
/// errs toward refusing.
fn points_at_prod_data(db_path: &std::path::Path, data_dir: &std::path::Path) -> bool {
    if db_path.starts_with(data_dir) {
        return true;
    }
    // Walk up to the deepest ancestor that exists and canonicalize it; the
    // db file and leaf dirs may not exist yet.
    let canon_data = data_dir.canonicalize();
    let mut probe = db_path;
    loop {
        if let Ok(real) = probe.canonicalize() {
            match &canon_data {
                Ok(cd) if real.starts_with(cd) => return true,
                // If the prod dir itself doesn't exist, compare against its
                // literal form (nothing to resolve to).
                Err(_) if real.starts_with(data_dir) => return true,
                _ => return false,
            }
        }
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p,
            _ => return false,
        }
    }
}

fn app_data_dir() -> Result<PathBuf, ConfigError> {
    Ok(dirs::data_dir()
        .ok_or(ConfigError::NoPlatformDirs)?
        .join("banshee"))
}

fn app_config_dir() -> Result<PathBuf, ConfigError> {
    Ok(dirs::config_dir()
        .ok_or(ConfigError::NoPlatformDirs)?
        .join("banshee"))
}

impl Config {
    /// Resolve configuration from the environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        let profile = match std::env::var("BANSHEE_PROFILE").as_deref() {
            Err(_) | Ok("prod") => Profile::Prod,
            Ok("dev") => Profile::Dev,
            Ok("test") => Profile::Test,
            Ok(other) => return Err(ConfigError::BadProfile(other.into())),
        };

        let env_db = std::env::var("BANSHEE_DB_PATH").ok().map(PathBuf::from);
        let env_port = match std::env::var("BANSHEE_PORT") {
            Ok(s) => Some(
                s.parse::<u16>()
                    .map_err(|_| ConfigError::BadPort("BANSHEE_PORT", s))?,
            ),
            Err(_) => None,
        };
        let env_key = std::env::var("BANSHEE_KEY_FILE").ok().map(PathBuf::from);
        // ADR-0008. A separate variable from BANSHEE_API_URL on purpose: that string
        // is what `host_is_loopback` parses to decide whether the key may be attached,
        // and percent-encoding a filesystem path into it would invite exactly the
        // confusion the gate exists to prevent.
        let env_socket = std::env::var(banshee_core::API_SOCKET_ENV)
            .ok()
            .map(PathBuf::from);

        let config = match profile {
            Profile::Test => {
                let db_path = env_db.ok_or(ConfigError::MissingTestEnv("BANSHEE_DB_PATH"))?;
                let data_dir = app_data_dir()?;
                // Compare RESOLVED paths, not literal strings: a symlink whose
                // literal path sits outside the prod dir but resolves inside it
                // would otherwise defeat this guard. Canonicalize the deepest
                // existing ancestor (the db file itself needn't exist yet) and
                // also keep the literal check for the `..`-traversal case where
                // nothing resolves.
                if points_at_prod_data(&db_path, &data_dir) {
                    return Err(ConfigError::TestPointsAtProdData(
                        db_path.display().to_string(),
                    ));
                }
                // Derived BEFORE the move, so the socket lands beside the test's own
                // database in whatever temp dir the harness made.
                let socket_path = env_socket.unwrap_or_else(|| db_path.with_extension("sock"));
                Self {
                    profile,
                    db_path,
                    port: env_port.ok_or(ConfigError::MissingTestEnv("BANSHEE_PORT"))?,
                    key_file: env_key.ok_or(ConfigError::MissingTestEnv("BANSHEE_KEY_FILE"))?,
                    socket_path,
                }
            }
            // Resolve platform dirs ONLY when the env override is absent:
            // `unwrap_or(app_data_dir()?…)` evaluated `app_data_dir()?` even
            // when `BANSHEE_DB_PATH`/`BANSHEE_KEY_FILE` were set, so an
            // explicit override still failed on a headless box where
            // `dirs::data_dir()` is None. `match` defers the fallible call.
            Profile::Dev => Self {
                profile,
                db_path: match env_db {
                    Some(p) => p,
                    None => app_data_dir()?.join("banshee-dev.db"),
                },
                port: env_port.unwrap_or(DEV_API_PORT),
                key_file: match env_key {
                    Some(p) => p,
                    None => app_config_dir()?.join("api_key"),
                },
                socket_path: match env_socket {
                    Some(p) => p,
                    // A different file from prod's, so a dev daemon and a prod daemon
                    // can run at once — the same reason DEV_API_PORT is a different
                    // literal.
                    None => app_config_dir()?.join("api-dev.sock"),
                },
            },
            Profile::Prod => Self {
                profile,
                db_path: match env_db {
                    Some(p) => p,
                    None => app_data_dir()?.join("banshee.db"),
                },
                port: env_port.unwrap_or(DEFAULT_API_PORT),
                key_file: match env_key {
                    Some(p) => p,
                    None => app_config_dir()?.join("api_key"),
                },
                socket_path: match env_socket {
                    Some(p) => p,
                    None => app_config_dir()?.join(banshee_core::API_SOCKET_FILE),
                },
            },
        };
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests mutate process-global state; serialize them.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        let all = [
            "BANSHEE_PROFILE",
            "BANSHEE_DB_PATH",
            "BANSHEE_PORT",
            "BANSHEE_KEY_FILE",
        ];
        let saved: Vec<_> = all.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        for k in all {
            unsafe { std::env::remove_var(k) };
        }
        for (k, v) in vars {
            if let Some(v) = v {
                unsafe { std::env::set_var(k, v) };
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => unsafe { std::env::set_var(k, v) },
                None => unsafe { std::env::remove_var(k) },
            }
        }
    }

    /// The prod default comes from `banshee_core::DEFAULT_API_PORT`, not from a
    /// literal here — and the literal is asserted too, so a careless bump to the
    /// core constant is a visible, deliberate change rather than a silent one.
    ///
    /// Mutation-proof: replace `DEFAULT_API_PORT` in `from_env` with any other
    /// number and this fails. That is the split-port-constant bug, caught.
    #[test]
    fn default_profile_is_prod_on_the_core_default_port() {
        with_env(&[], || {
            let c = Config::from_env().unwrap();
            assert_eq!(c.profile, Profile::Prod);
            assert_eq!(c.port, DEFAULT_API_PORT);
            assert_eq!(DEFAULT_API_PORT, 18769, "documented in the README");
            assert!(c.db_path.ends_with("banshee/banshee.db"));
        });
    }

    /// The dev profile takes the OTHER core constant. Mutation-proof: give the dev
    /// arm `DEFAULT_API_PORT` and this fails — the state where `make start` on the
    /// dev profile quietly fights the prod daemon for a port.
    #[test]
    fn the_dev_profile_uses_the_core_dev_port() {
        with_env(&[("BANSHEE_PROFILE", Some("dev"))], || {
            let c = Config::from_env().unwrap();
            assert_eq!(c.profile, Profile::Dev);
            assert_eq!(c.port, DEV_API_PORT);
            assert_ne!(c.port, DEFAULT_API_PORT);
            assert!(c.db_path.ends_with("banshee/banshee-dev.db"));
        });
    }

    #[test]
    fn unknown_profile_is_rejected() {
        with_env(&[("BANSHEE_PROFILE", Some("staging"))], || {
            assert!(matches!(
                Config::from_env(),
                Err(ConfigError::BadProfile(_))
            ));
        });
    }

    #[test]
    fn test_profile_requires_explicit_db_port_and_key() {
        with_env(&[("BANSHEE_PROFILE", Some("test"))], || {
            assert!(matches!(
                Config::from_env(),
                Err(ConfigError::MissingTestEnv("BANSHEE_DB_PATH"))
            ));
        });
    }

    #[test]
    fn test_profile_refuses_prod_data_dir() {
        let prod_db = app_data_dir().unwrap().join("anything.db");
        let prod_db = prod_db.to_str().unwrap().to_string();
        with_env(
            &[
                ("BANSHEE_PROFILE", Some("test")),
                ("BANSHEE_DB_PATH", Some(&prod_db)),
                ("BANSHEE_PORT", Some("18999")),
                ("BANSHEE_KEY_FILE", Some("/tmp/k")),
            ],
            || {
                assert!(matches!(
                    Config::from_env(),
                    Err(ConfigError::TestPointsAtProdData(_))
                ));
            },
        );
    }

    #[test]
    fn test_profile_refuses_symlink_into_prod_data() {
        // A symlink whose literal path is outside the prod dir but which
        // RESOLVES inside it must still be refused (adversarial finding,
        // 2026-08-20). Skipped only if the prod data dir cannot be created.
        let Ok(prod) = app_data_dir() else { return };
        if std::fs::create_dir_all(&prod).is_err() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("sneaky");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&prod, &link).unwrap();
        let db = link.join("evil.db");
        let db = db.to_str().unwrap().to_string();
        with_env(
            &[
                ("BANSHEE_PROFILE", Some("test")),
                ("BANSHEE_DB_PATH", Some(&db)),
                ("BANSHEE_PORT", Some("0")),
                ("BANSHEE_KEY_FILE", Some("/tmp/banshee-symlink-test-key")),
            ],
            || {
                assert!(
                    matches!(
                        Config::from_env(),
                        Err(ConfigError::TestPointsAtProdData(_))
                    ),
                    "a symlink resolving into the prod data dir must be refused"
                );
            },
        );
    }

    #[test]
    fn test_profile_with_full_env_resolves() {
        with_env(
            &[
                ("BANSHEE_PROFILE", Some("test")),
                ("BANSHEE_DB_PATH", Some("/tmp/banshee-test/banshee.db")),
                ("BANSHEE_PORT", Some("0")),
                ("BANSHEE_KEY_FILE", Some("/tmp/banshee-test/api_key")),
            ],
            || {
                let c = Config::from_env().unwrap();
                assert_eq!(c.profile, Profile::Test);
                assert_eq!(c.port, 0);
            },
        );
    }

    #[test]
    fn garbage_port_is_rejected_not_defaulted() {
        with_env(&[("BANSHEE_PORT", Some("many"))], || {
            assert!(matches!(Config::from_env(), Err(ConfigError::BadPort(..))));
        });
    }
}
