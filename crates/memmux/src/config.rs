//! User configuration from `<root>/config.toml` (SUM-133).
//!
//! Currently just the pane leader key. Loading never fails the UI: a missing file, unreadable
//! file, malformed TOML, or unrecognized value all fall back to defaults.

use crate::app::Prefix;
use serde::Deserialize;
use std::path::Path;

/// Resolved runtime configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Config {
    /// The pane-command leader key (default `Ctrl-b`).
    pub prefix: Prefix,
}

/// The on-disk shape (`~/.memmux/config.toml`).
#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    /// Pane leader key spec, e.g. `"ctrl-b"`.
    prefix: Option<String>,
}

impl Config {
    /// Load `<root>/config.toml`, applying defaults for anything missing or invalid.
    pub fn load(root: &Path) -> Config {
        let raw = std::fs::read_to_string(root.join("config.toml"))
            .ok()
            .and_then(|s| toml::from_str::<RawConfig>(&s).ok())
            .unwrap_or_default();
        Config {
            prefix: raw
                .prefix
                .as_deref()
                .and_then(Prefix::parse)
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "memmux-cfg-{}-{:?}",
            std::process::id(),
            body.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
        dir
    }

    #[test]
    fn missing_file_uses_defaults() {
        let cfg = Config::load(std::path::Path::new("/no/such/memmux/root"));
        assert_eq!(cfg.prefix, Prefix::default()); // ctrl-b
    }

    #[test]
    fn reads_a_custom_prefix() {
        let dir = write_config("prefix = \"ctrl-a\"\n");
        let cfg = Config::load(&dir);
        assert_eq!(
            cfg.prefix,
            Prefix {
                ctrl: true,
                ch: 'a'
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn garbage_prefix_falls_back_to_default() {
        let dir = write_config("prefix = \"not-a-key!!\"\n");
        let cfg = Config::load(&dir);
        assert_eq!(cfg.prefix, Prefix::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
