//! Remote builders from machines files and the `builders` setting.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Machine {
    pub uri: String,
    pub ssh_key: Option<String>,
    pub max_jobs: u32,
    pub host_key: Option<String>,
}

impl Machine {
    pub fn is_local(&self) -> bool {
        matches!(self.uri.as_str(), "localhost" | "local" | "auto" | "daemon")
            || self.uri.starts_with("local?")
            || self.uri.starts_with("unix://")
    }

    /// Prefix of this machine's slot lock files in `current-load/`; Nix
    /// replaces `/` with `_` in the store URI.
    pub fn slot_prefix(&self) -> String {
        format!("{}-", self.uri.replace('/', "_"))
    }

    /// The store URI with the entry's ssh key and host key as parameters.
    pub fn ping_uri(&self) -> String {
        let mut params = Vec::new();
        if let Some(k) = &self.ssh_key {
            params.push(format!("ssh-key={k}"));
        }
        if let Some(h) = &self.host_key {
            params.push(format!("base64-ssh-public-host-key={h}"));
        }
        if params.is_empty() {
            return self.uri.clone();
        }
        let sep = if self.uri.contains('?') { '&' } else { '?' };
        format!("{}{sep}{}", self.uri, params.join("&"))
    }
}

/// Entries separated by newlines or `;`, `#` comments. Fields: URI, systems,
/// ssh key, max jobs, speed factor, supported features, mandatory features,
/// base64 host key; `-` means unset.
pub fn parse(text: &str) -> Vec<Machine> {
    text.split(['\n', ';']).filter_map(parse_entry).collect()
}

fn field(f: &[&str], i: usize) -> Option<String> {
    f.get(i)
        .filter(|v| !v.is_empty() && **v != "-")
        .map(|v| v.to_string())
}

fn parse_entry(entry: &str) -> Option<Machine> {
    let entry = entry.split('#').next()?.trim();
    let f: Vec<&str> = entry.split_whitespace().collect();
    let raw = *f.first()?;
    let uri = if raw.contains("://") || matches!(raw, "localhost" | "local" | "auto" | "daemon") {
        raw.to_string()
    } else {
        format!("ssh://{raw}")
    };
    Some(Machine {
        uri,
        ssh_key: field(&f, 2),
        max_jobs: f.get(3).and_then(|n| n.parse().ok()).unwrap_or(1),
        host_key: field(&f, 7),
    })
}

fn read(path: &Path) -> Vec<Machine> {
    fs::read_to_string(path)
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Machines from the `builders` setting (`@file` parts are read) followed by
/// `extra_files`; the first entry for each URI wins.
pub fn load(builders: Option<&str>, extra_files: &[PathBuf]) -> Vec<Machine> {
    let mut found = Vec::new();
    for part in builders.unwrap_or("").split(';') {
        let part = part.trim();
        match part.strip_prefix('@') {
            Some(file) => found.extend(read(Path::new(file))),
            None => found.extend(parse(part)),
        }
    }
    for f in extra_files {
        found.extend(read(f));
    }
    let mut out: Vec<Machine> = Vec::new();
    for m in found {
        if !out.iter().any(|o| o.uri == m.uri) {
            out.push(m);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::write;

    const FREDDIE: &str = "ssh-ng://remotebuild@juicy-j.dab-ling.ts.net x86_64-linux /root/.ssh/remotebuild 32 4 nixos-test,big-parallel,kvm - c3NoLWVkMjU1MTk=\nssh-ng://eu.nixbuild.net aarch64-linux /run/agenix/nixbuild-dot-net.ssh-key 100 1 benchmark,big-parallel - c3NoLWVk\n";

    #[test]
    fn parses_entries() {
        let ms = parse(FREDDIE);
        assert_eq!(ms.len(), 2);
        assert_eq!(ms[0].uri, "ssh-ng://remotebuild@juicy-j.dab-ling.ts.net");
        assert_eq!(ms[0].ssh_key.as_deref(), Some("/root/.ssh/remotebuild"));
        assert_eq!(ms[0].max_jobs, 32);
        assert_eq!(ms[0].host_key.as_deref(), Some("c3NoLWVkMjU1MTk="));
        assert_eq!(
            ms[0].slot_prefix(),
            "ssh-ng:__remotebuild@juicy-j.dab-ling.ts.net-"
        );
        assert_eq!(
            ms[0].ping_uri(),
            "ssh-ng://remotebuild@juicy-j.dab-ling.ts.net?ssh-key=/root/.ssh/remotebuild&base64-ssh-public-host-key=c3NoLWVkMjU1MTk="
        );
    }

    #[test]
    fn defaults_local_comments_and_separators() {
        let ms = parse("localhost x86_64-linux,i686-linux - 16 1 kvm - -\n# comment\nbuilder x86_64-linux ; ssh://other?a=b");
        assert!(ms[0].is_local());
        assert_eq!(ms[0].max_jobs, 16);
        assert_eq!(ms[0].ssh_key, None);
        assert_eq!(ms[1].uri, "ssh://builder");
        assert_eq!(ms[1].max_jobs, 1);
        assert!(!ms[1].is_local());
        assert_eq!(ms[2].uri, "ssh://other?a=b");
        assert_eq!(ms[2].ping_uri(), "ssh://other?a=b");
    }

    #[test]
    fn loads_setting_files_and_dedups() {
        let d = tempfile::tempdir().unwrap();
        let machines = d.path().join("machines");
        let hydra = d.path().join("hydra-machines");
        write(&machines, FREDDIE);
        write(
            &hydra,
            "localhost x86_64-linux - 16 1\nssh-ng://eu.nixbuild.net aarch64-linux - 5 1\n",
        );
        let ms = load(Some(&format!("@{}", machines.display())), &[hydra]);
        let uris: Vec<&str> = ms.iter().map(|m| m.uri.as_str()).collect();
        assert_eq!(
            uris,
            vec![
                "ssh-ng://remotebuild@juicy-j.dab-ling.ts.net",
                "ssh-ng://eu.nixbuild.net",
                "localhost"
            ]
        );
        assert_eq!(ms[1].max_jobs, 100, "first entry wins");
        assert!(load(None, &[]).is_empty());
    }
}
