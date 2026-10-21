//! `systemctl show` output and the logs of GC and optimise runs.

use std::collections::HashMap;

/// `systemctl show` key=value lines.
pub fn parse_show(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// A `--timestamp=unix` value such as `@1792474157`; empty or `n/a` is `None`.
pub fn unix_ts(v: Option<&String>) -> Option<f64> {
    v?.strip_prefix('@')?.parse().ok()
}

#[derive(Debug, Default, PartialEq)]
pub struct RunLog {
    pub freed_bytes: Option<f64>,
    pub deleted_paths: Option<f64>,
    pub hardlink_saved_bytes: Option<f64>,
}

fn size(num: &str, unit: &str) -> Option<f64> {
    let n: f64 = num.parse().ok()?;
    let mult = match unit {
        "B" | "bytes" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        "TiB" => 1024f64.powi(4),
        _ => return None,
    };
    Some(n * mult)
}

/// Parses one `nix-collect-garbage` / `nix-store --gc` or `nix-store --optimise` run.
pub fn parse_log(text: &str) -> RunLog {
    let mut log = RunLog::default();
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        // "0 store paths deleted, 0.0 KiB freed"
        if let Some(i) = t
            .windows(3)
            .position(|w| w == ["store", "paths", "deleted,"])
        {
            log.deleted_paths = i.checked_sub(1).and_then(|j| t[j].parse().ok());
            if let (Some(n), Some(u)) = (t.get(i + 3), t.get(i + 4)) {
                if let Some(b) = size(n, u) {
                    *log.freed_bytes.get_or_insert(0.0) += b;
                }
            }
        }
        // "2.2 GiB freed by hard-linking 618926 files"
        if let Some(i) = t
            .windows(3)
            .position(|w| w == ["freed", "by", "hard-linking"])
        {
            if i >= 2 {
                if let Some(b) = size(t[i - 2], t[i - 1]) {
                    *log.freed_bytes.get_or_insert(0.0) += b;
                }
            }
        }
        // "note: hard linking is currently saving 71.2 GiB" (Determinate)
        // "note: currently hard linking saves 71.20 MiB" (upstream)
        if let Some(i) = t.iter().position(|w| *w == "saving" || *w == "saves") {
            if let (Some(n), Some(u)) = (t.get(i + 1), t.get(i + 2)) {
                log.hardlink_saved_bytes = size(n, u);
            }
        }
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    #[test]
    fn parses_gc_run() {
        let log = parse_log("removing old generations of profile /nix/var/nix/profiles/system\nfinding garbage collector roots...\ndeleting garbage...\ndeleting unused links...\nnote: hard linking is currently saving 71.2 GiB\n0 store paths deleted, 0.0 KiB freed\n");
        assert_eq!(log.deleted_paths, Some(0.0));
        assert_eq!(log.freed_bytes, Some(0.0));
        assert_eq!(log.hardlink_saved_bytes, Some(71.2 * GIB));
    }

    #[test]
    fn parses_gc_with_deletions_and_upstream_saving_line() {
        let log = parse_log(
            "note: currently hard linking saves 1.50 MiB\n12 store paths deleted, 3.00 MiB freed\n",
        );
        assert_eq!(log.deleted_paths, Some(12.0));
        assert_eq!(log.freed_bytes, Some(3.0 * 1024.0 * 1024.0));
        assert_eq!(log.hardlink_saved_bytes, Some(1.5 * 1024.0 * 1024.0));
    }

    #[test]
    fn parses_optimise_run() {
        let log = parse_log("2.2 GiB freed by hard-linking 618926 files\n");
        assert_eq!(log.freed_bytes, Some(2.2 * GIB));
        assert_eq!(log.deleted_paths, None);
    }

    #[test]
    fn parses_systemctl_show() {
        let p = parse_show("LoadState=loaded\nResult=success\nInvocationID=3e10956031c046058871baf19a2257d3\nExecMainStartTimestamp=@1792400401\nExecMainExitTimestamp=@1792400446\n");
        assert_eq!(p["LoadState"], "loaded");
        assert_eq!(unix_ts(p.get("ExecMainStartTimestamp")), Some(1792400401.0));
        assert_eq!(unix_ts(p.get("Missing")), None);
        let empty = parse_show("ExecMainStartTimestamp=\n");
        assert_eq!(unix_ts(empty.get("ExecMainStartTimestamp")), None);
    }
}
