//! cgroup v2 metrics for the Nix daemon's unit.

use crate::metrics::Family;
use std::fs;
use std::path::{Path, PathBuf};

/// The cgroup v2 directory of `pid`, if it is in the unified hierarchy.
pub fn dir_of(proc_dir: &Path, cgroup_root: &Path, pid: u32) -> Option<PathBuf> {
    let text = fs::read_to_string(proc_dir.join(pid.to_string()).join("cgroup")).ok()?;
    let rel = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    let dir = cgroup_root.join(rel.trim_start_matches('/'));
    // With use-cgroups, upstream moves the daemon into `<unit>/nix-daemon`
    // and builds into sibling `nix-build-*` cgroups; count the whole unit.
    if dir.file_name().is_some_and(|n| n == "nix-daemon") {
        return dir.parent().map(Path::to_path_buf);
    }
    Some(dir)
}

fn read(dir: &Path, file: &str) -> Option<String> {
    fs::read_to_string(dir.join(file)).ok()
}

fn keyed(text: &str, key: &str) -> Option<f64> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(' ')?;
        if k == key {
            v.trim().parse().ok()
        } else {
            None
        }
    })
}

/// Each missing file omits only its own metric.
pub fn families(dir: &Path) -> Vec<Family> {
    let mut out = Vec::new();
    if let Some(stat) = read(dir, "cpu.stat") {
        let mut f = Family::counter(
            "nix_daemon_cgroup_cpu_seconds_total",
            "CPU time used in the Nix daemon unit's cgroup, builds included.",
        );
        for (mode, key) in [("user", "user_usec"), ("system", "system_usec")] {
            if let Some(v) = keyed(&stat, key) {
                f.push(&[("mode", mode)], v / 1e6);
            }
        }
        out.push(f);
    }
    for (file, name, help) in [
        (
            "memory.current",
            "nix_daemon_cgroup_memory_bytes",
            "Memory charged to the Nix daemon unit's cgroup.",
        ),
        (
            "memory.peak",
            "nix_daemon_cgroup_memory_peak_bytes",
            "Peak memory charged to the Nix daemon unit's cgroup.",
        ),
    ] {
        if let Some(v) = read(dir, file).and_then(|t| t.trim().parse::<f64>().ok()) {
            out.push(Family::gauge(name, help).with(&[], v));
        }
    }
    if let Some(io) = read(dir, "io.stat") {
        let (mut r, mut w) = (0.0, 0.0);
        for kv in io.split_whitespace() {
            if let Some(v) = kv.strip_prefix("rbytes=") {
                r += v.parse::<f64>().unwrap_or(0.0);
            }
            if let Some(v) = kv.strip_prefix("wbytes=") {
                w += v.parse::<f64>().unwrap_or(0.0);
            }
        }
        out.push(
            Family::counter(
                "nix_daemon_cgroup_io_bytes_total",
                "Block I/O in the Nix daemon unit's cgroup.",
            )
            .with(&[("direction", "read")], r)
            .with(&[("direction", "write")], w),
        );
    }
    let mut psi = Family::counter(
        "nix_daemon_pressure_seconds_total",
        "Time tasks in the Nix daemon unit's cgroup were stalled on a resource (PSI).",
    );
    for resource in ["cpu", "memory", "io"] {
        let Some(text) = read(dir, &format!("{resource}.pressure")) else {
            continue;
        };
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let Some(kind) = it.next() else {
                continue;
            };
            let total = it
                .find_map(|kv| kv.strip_prefix("total="))
                .and_then(|v| v.parse::<f64>().ok());
            if let Some(total) = total {
                psi.push(&[("resource", resource), ("kind", kind)], total / 1e6);
            }
        }
    }
    out.push(psi);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::testutil::write;

    #[test]
    fn finds_unit_cgroup() {
        let d = tempfile::tempdir().unwrap();
        write(
            &d.path().join("proc/100/cgroup"),
            "1:net_cls:/\n0::/system.slice/nix-daemon.service\n",
        );
        assert_eq!(
            dir_of(&d.path().join("proc"), Path::new("/sys/fs/cgroup"), 100),
            Some(PathBuf::from(
                "/sys/fs/cgroup/system.slice/nix-daemon.service"
            ))
        );
        assert_eq!(
            dir_of(&d.path().join("proc"), Path::new("/sys/fs/cgroup"), 101),
            None
        );
        write(
            &d.path().join("proc/102/cgroup"),
            "0::/system.slice/nix-daemon.service/nix-daemon\n",
        );
        assert_eq!(
            dir_of(&d.path().join("proc"), Path::new("/sys/fs/cgroup"), 102),
            Some(PathBuf::from(
                "/sys/fs/cgroup/system.slice/nix-daemon.service"
            ))
        );
    }

    #[test]
    fn reads_counters_and_skips_missing_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        write(
            &p.join("cpu.stat"),
            "usage_usec 1750000\nuser_usec 1500000\nsystem_usec 250000\n",
        );
        write(&p.join("memory.current"), "1048576\n");
        write(
            &p.join("io.stat"),
            "259:0 rbytes=100 wbytes=200 rios=1 wios=2\n8:0 rbytes=1 wbytes=2 rios=0 wios=0\n",
        );
        write(&p.join("cpu.pressure"), "some avg10=0.00 avg60=0.00 avg300=0.00 total=1500000\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
        let text = render(&families(p));
        assert!(
            text.contains("nix_daemon_cgroup_cpu_seconds_total{mode=\"user\"} 1.5\n"),
            "{text}"
        );
        assert!(text.contains("nix_daemon_cgroup_cpu_seconds_total{mode=\"system\"} 0.25\n"));
        assert!(text.contains("nix_daemon_cgroup_memory_bytes 1048576\n"));
        assert!(!text.contains("nix_daemon_cgroup_memory_peak_bytes"));
        assert!(text.contains("nix_daemon_cgroup_io_bytes_total{direction=\"read\"} 101\n"));
        assert!(text.contains("nix_daemon_cgroup_io_bytes_total{direction=\"write\"} 202\n"));
        assert!(text
            .contains("nix_daemon_pressure_seconds_total{resource=\"cpu\",kind=\"some\"} 1.5\n"));
        assert!(
            text.contains("nix_daemon_pressure_seconds_total{resource=\"cpu\",kind=\"full\"} 0\n")
        );
        assert!(!text.contains("resource=\"memory\""));
    }
}
