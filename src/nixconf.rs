//! The daemon's settings, from `nix config show`.

use crate::cmd;
use anyhow::Result;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct NixConfig {
    settings: BTreeMap<String, String>,
}

impl NixConfig {
    pub fn parse(text: &str) -> Self {
        let settings = text
            .lines()
            .filter_map(|l| l.split_once(" ="))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        NixConfig { settings }
    }

    pub fn load() -> Result<Self> {
        let text = cmd::output(
            Command::new("nix").args([
                "--extra-experimental-features",
                "nix-command",
                "config",
                "show",
            ]),
            Duration::from_secs(30),
        )?;
        Ok(Self::parse(&text))
    }

    /// A setting's value; empty values count as unset.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn flag(&self, key: &str) -> bool {
        self.get(key) == Some("true")
    }

    pub fn number(&self, key: &str) -> Option<u64> {
        self.get(key)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "auto-allocate-uids = true\nbuild-users-group = \nbuilders = @/etc/nix/machines\nid-count = 8388608\nmax-jobs = 32\nstart-id = 872415232\nkeep-derivations = true\nkeep-outputs = false\n";

    #[test]
    fn parses_settings() {
        let c = NixConfig::parse(SAMPLE);
        assert!(c.flag("auto-allocate-uids"));
        assert!(c.flag("keep-derivations"));
        assert!(!c.flag("keep-outputs"));
        assert!(!c.flag("missing"));
        assert_eq!(c.get("build-users-group"), None);
        assert_eq!(c.get("builders"), Some("@/etc/nix/machines"));
        assert_eq!(c.number("max-jobs"), Some(32));
        assert_eq!(c.number("start-id"), Some(872415232));
    }
}
