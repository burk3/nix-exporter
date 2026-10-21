use crate::active_builds;
use crate::attribution::Attributor;
use crate::buildset::{build_pids, BuildUids};
use crate::cgroup;
use crate::cmd;
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::nettcp::TcpTable;
use crate::procs::{self, find_daemon, ProcTable};
use crate::users::Users;
use anyhow::Result;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

pub struct Daemon {
    interval: Duration,
    /// (main daemon pid, its (version, variant) if the probe worked), refreshed
    /// when the daemon restarts.
    version: Option<(u32, Option<(String, &'static str)>)>,
}

impl Daemon {
    pub fn new(interval: Duration) -> Self {
        Daemon {
            interval,
            version: None,
        }
    }
}

/// First line of `--version`, e.g. `nix-daemon (Determinate Nix 3.23.3) 2.35.3`.
pub fn parse_version(text: &str) -> Option<(String, &'static str)> {
    let line = text.lines().next()?;
    let version = line.split_whitespace().last()?.to_string();
    let variant = if line.contains("Determinate") {
        "determinate"
    } else {
        "upstream"
    };
    Some((version, variant))
}

impl Collector for Daemon {
    fn name(&self) -> &'static str {
        "daemon"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        // Without the config only max-jobs and the build-UID set are lost.
        let cfg = ctx.nix_config().ok();
        let procs = ProcTable::read(&ctx.paths.proc_dir);
        let users = Users::read(&ctx.paths.etc_dir);
        let d = find_daemon(&procs);
        let mut out =
            vec![
                Family::gauge("nix_daemon_up", "Whether the Nix daemon process was found.")
                    .with(&[], if d.main.is_some() { 1.0 } else { 0.0 }),
            ];
        if let Some(n) = cfg.as_ref().and_then(|c| c.number("max-jobs")) {
            out.push(
                Family::gauge("nix_daemon_max_jobs", "The max-jobs setting.").with(&[], n as f64),
            );
        }
        let Some(main) = d.main else {
            return Ok(out);
        };

        if self.version.as_ref().is_none_or(|(pid, _)| *pid != main) {
            let exe = ctx.paths.proc_dir.join(main.to_string()).join("exe");
            let text = cmd::output(Command::new(exe).arg("--version"), Duration::from_secs(10))
                .unwrap_or_default();
            // Cached even when the probe fails: retried only once the daemon restarts.
            self.version = Some((main, parse_version(&text)));
        }
        if let Some((_, Some((version, variant)))) = &self.version {
            out.push(
                Family::gauge("nix_daemon_info", "Nix daemon version.")
                    .with(&[("version", version), ("variant", variant)], 1.0),
            );
        }
        if let (Some(boot), Some(p)) = (procs::boot_time(&ctx.paths.proc_dir), procs.get(main)) {
            out.push(
                Family::gauge(
                    "nix_daemon_start_time_seconds",
                    "When the Nix daemon started.",
                )
                .with(&[], procs::start_time_seconds(p, boot)),
            );
        }
        out.push(
            Family::gauge(
                "nix_daemon_connections",
                "Open daemon connections (worker processes).",
            )
            .with(&[], d.workers.len() as f64),
        );

        let tcp = TcpTable::read(&ctx.paths.proc_dir);
        let attr = Attributor {
            procs: &procs,
            users: &users,
            tcp: &tcp,
            resolver: &ctx.resolver,
        };
        let mut sessions: BTreeMap<(String, String), f64> = BTreeMap::new();
        for w in &d.workers {
            let client = procs
                .get(*w)
                .and_then(|p| p.worker_client_pid())
                .and_then(|c| procs.get(c));
            let Some(client) = client.filter(|c| c.is_remote_server()) else {
                continue;
            };
            // A server with no ssh peer is local, e.g. Hydra's `localhost`
            // machine running `nix-store --serve --write` under the queue runner.
            let Some(peer) = attr.peer_of(client.pid) else {
                continue;
            };
            *sessions.entry((users.name(client.uid), peer)).or_default() += 1.0;
        }
        let mut f = Family::gauge(
            "nix_daemon_remote_sessions",
            "Daemon connections from remote Nix clients over ssh.",
        );
        for ((user, client), n) in &sessions {
            f.push(&[("user", user), ("client", client)], *n);
        }
        out.push(f);

        let uids = cfg
            .as_ref()
            .map(|c| BuildUids::from_config(c, &users))
            .unwrap_or_default();
        let mains: Vec<u32> = active_builds::read_dir(&ctx.paths.active_builds())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| b.main_pid)
            .collect();
        let builds = build_pids(&procs, &mains, &uids);
        let rss = |pids: &mut dyn Iterator<Item = u32>| -> f64 {
            pids.filter_map(|p| procs.get(p))
                .map(|p| p.rss_bytes as f64)
                .sum()
        };
        let daemon_rss = rss(&mut std::iter::once(main).chain(d.workers.iter().copied()));
        let builds_rss = rss(&mut builds.iter().copied());
        out.push(
            Family::gauge(
                "nix_daemon_memory_bytes",
                "Resident memory of the daemon and of running builds.",
            )
            .with(&[("role", "daemon")], daemon_rss)
            .with(&[("role", "builds")], builds_rss),
        );

        if let Some(dir) = cgroup::dir_of(&ctx.paths.proc_dir, &ctx.paths.cgroup_root, main) {
            out.extend(cgroup::families(&dir));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::paths::Paths;
    use crate::testutil::{fake_proc, fake_socket_fd, write};

    #[test]
    fn parses_versions() {
        assert_eq!(
            parse_version("nix-daemon (Determinate Nix 3.23.3) 2.35.3\n"),
            Some(("2.35.3".to_string(), "determinate"))
        );
        assert_eq!(
            parse_version("nix-daemon (Nix) 2.34.9\n"),
            Some(("2.34.9".to_string(), "upstream"))
        );
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn reports_daemon_state() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let proc_dir = root.join("proc");
        let p = &proc_dir;
        fake_proc(p, 1, 0, 0, "systemd", &["systemd"]);
        fake_proc(p, 100, 1, 0, "nix-daemon", &["nix-daemon", "--daemon"]);
        // a remote client over ssh-ng and its worker
        fake_proc(
            p,
            290,
            1,
            1001,
            "sshd-session",
            &["sshd-session: remotebuild@notty"],
        );
        fake_proc(p, 300, 290, 1001, "nix-daemon", &["nix-daemon", "--stdio"]);
        fake_proc(p, 200, 100, 0, "nix-daemon", &["nix-daemon", "300"]);
        fake_socket_fd(p, 290, 7, 11035730);
        write(&p.join("net/tcp"), "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0EA57364:0016 37924A64:B354 01 00000000:00000000 02:000A3B2C 00000000     0        0 11035730 2 0000000000000000 20 4 30 10 -1\n");
        // Hydra's local `nix-store --serve` (no sshd ancestor) and its worker
        fake_proc(p, 500, 1, 2000, "hydra-queue-run", &["hydra-queue-runner"]);
        fake_proc(
            p,
            510,
            500,
            2000,
            "nix-store",
            &["nix-store", "--serve", "--write"],
        );
        fake_proc(p, 210, 100, 0, "nix-daemon", &["nix-daemon", "510"]);
        // a build running as an auto-allocated UID under the worker
        fake_proc(p, 400, 200, 872415232, "bash", &["bash"]);
        write(&p.join("stat"), "btime 1792400000\n");
        write(
            &p.join("100/cgroup"),
            "0::/system.slice/nix-daemon.service\n",
        );
        write(
            &root.join("cgroup/system.slice/nix-daemon.service/cpu.stat"),
            "user_usec 2000000\nsystem_usec 1000000\n",
        );
        write(
            &root.join("etc/passwd"),
            "root:x:0:0::/:/x\nremotebuild:x:1001:100::/:/x\nhydra-queue-runner:x:2000:2000::/:/x\n",
        );
        let paths = Paths {
            proc_dir: proc_dir.clone(),
            cgroup_root: root.join("cgroup"),
            state_dir: root.join("state"),
            etc_dir: root.join("etc"),
            ..Paths::default()
        };
        let ctx = Ctx::for_test(
            paths,
            "max-jobs = 32\nauto-allocate-uids = true\nstart-id = 872415232\nid-count = 8388608\n",
        );
        let text = render(&Daemon::new(Duration::from_secs(1)).collect(&ctx).unwrap());
        assert!(text.contains("nix_daemon_up 1\n"), "{text}");
        assert!(text.contains("nix_daemon_max_jobs 32\n"));
        assert!(text.contains("nix_daemon_connections 2\n"));
        assert!(!text.contains("hydra-queue-runner"), "{text}");
        assert!(text.contains(
            "nix_daemon_remote_sessions{user=\"remotebuild\",client=\"100.74.146.55\"} 1\n"
        ));
        assert!(text.contains("nix_daemon_memory_bytes{role=\"daemon\"} 3072000\n"));
        assert!(text.contains("nix_daemon_memory_bytes{role=\"builds\"} 1024000\n"));
        assert!(text.contains("nix_daemon_cgroup_cpu_seconds_total{mode=\"user\"} 2\n"));
        assert!(text.contains("nix_daemon_start_time_seconds "));
    }

    #[test]
    fn failed_version_probe_is_retried_only_on_restart() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("proc");
        let calls = d.path().join("calls");
        let probe = d.path().join("probe");
        write(
            &probe,
            &format!("#!/bin/sh\necho x >> '{}'\nexit 1\n", calls.display()),
        );
        std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let daemon = |pid: u32| {
            fake_proc(&p, pid, 1, 0, "nix-daemon", &["nix-daemon", "--daemon"]);
            std::os::unix::fs::symlink(&probe, p.join(pid.to_string()).join("exe")).unwrap();
        };
        let count = || {
            std::fs::read_to_string(&calls)
                .unwrap_or_default()
                .lines()
                .count()
        };
        fake_proc(&p, 1, 0, 0, "systemd", &["systemd"]);
        daemon(100);
        let paths = Paths {
            proc_dir: p.clone(),
            ..Paths::default()
        };
        let ctx = Ctx::for_test(paths, "");
        let mut c = Daemon::new(Duration::from_secs(1));
        let text = render(&c.collect(&ctx).unwrap());
        assert!(!text.contains("nix_daemon_info"), "{text}");
        c.collect(&ctx).unwrap();
        assert_eq!(count(), 1);
        // The daemon restarts under a new pid: probe again.
        std::fs::remove_dir_all(p.join("100")).unwrap();
        daemon(101);
        c.collect(&ctx).unwrap();
        assert_eq!(count(), 2);
    }

    #[test]
    fn reports_without_config() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("proc");
        fake_proc(&p, 1, 0, 0, "systemd", &["systemd"]);
        fake_proc(&p, 100, 1, 0, "nix-daemon", &["nix-daemon", "--daemon"]);
        let paths = Paths {
            proc_dir: p.clone(),
            ..Paths::default()
        };
        let ctx = Ctx::for_test_without_config(paths);
        assert!(ctx.nix_config().is_err());
        let text = render(&Daemon::new(Duration::from_secs(1)).collect(&ctx).unwrap());
        assert!(text.contains("nix_daemon_up 1\n"), "{text}");
        assert!(text.contains("nix_daemon_connections 0\n"), "{text}");
        assert!(!text.contains("nix_daemon_max_jobs"), "{text}");
    }

    #[test]
    fn no_daemon() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            proc_dir: d.path().to_path_buf(),
            ..Paths::default()
        };
        let ctx = Ctx::for_test(paths, "");
        let text = render(&Daemon::new(Duration::from_secs(1)).collect(&ctx).unwrap());
        assert_eq!(text, "# HELP nix_daemon_up Whether the Nix daemon process was found.\n# TYPE nix_daemon_up gauge\nnix_daemon_up 0\n");
    }
}
