//! Determinate Nix's `active-builds/` directory: one JSON file per local build.

use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActiveBuild {
    pub nix_pid: u32,
    pub main_pid: Option<u32>,
    pub start_time: Option<f64>,
    pub client_pid: Option<u32>,
    pub client_uid: Option<u32>,
    pub cgroup: Option<String>,
}

/// `None` when the directory does not exist (Nix without build tracking).
/// Unreadable or malformed entries are skipped.
pub fn read_dir(dir: &Path) -> Option<Vec<ActiveBuild>> {
    let entries = fs::read_dir(dir).ok()?;
    Some(
        entries
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::write;

    pub const JUICY_J: &str = r#"{"cgroup":"/sys/fs/cgroup/system.slice/nix-daemon.service/nix-build-uid-872415232","clientPid":null,"clientUid":null,"derivation":"p0l886hl0qpyvs9mb3clz8allx6a5yl9-probe-uidrange.drv","mainPid":1355665,"mainUser":{"name":null,"uid":872415232},"nixPid":1355628,"startTime":1792482889.0,"stime":0.04,"type":"build","utime":3.47}"#;

    #[test]
    fn reads_entries_and_skips_garbage() {
        let d = tempfile::tempdir().unwrap();
        write(&d.path().join("1355628-1"), JUICY_J);
        write(&d.path().join("junk"), "not json");
        let builds = read_dir(d.path()).unwrap();
        assert_eq!(builds.len(), 1);
        let b = &builds[0];
        assert_eq!(b.nix_pid, 1355628);
        assert_eq!(b.main_pid, Some(1355665));
        assert_eq!(b.start_time, Some(1792482889.0));
        assert_eq!(b.client_pid, None);
        assert!(b
            .cgroup
            .as_deref()
            .unwrap()
            .ends_with("nix-build-uid-872415232"));
    }

    #[test]
    fn missing_directory_is_none() {
        assert!(read_dir(Path::new("/nonexistent/active-builds")).is_none());
    }
}
