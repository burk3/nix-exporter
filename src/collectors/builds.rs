use crate::active_builds;
use crate::attribution::{Attributor, Origin};
use crate::buildset::{self, BuildUids};
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::nettcp::TcpTable;
use crate::procs::{self, ProcTable};
use crate::users::Users;
use anyhow::Result;
use std::collections::BTreeMap;
use std::time::Duration;

pub struct Builds {
    pub interval: Duration,
}

impl Collector for Builds {
    fn name(&self) -> &'static str {
        "builds"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let procs = ProcTable::read(&ctx.paths.proc_dir);
        let users = Users::read(&ctx.paths.etc_dir);
        let tcp = TcpTable::read(&ctx.paths.proc_dir);
        let attr = Attributor {
            procs: &procs,
            users: &users,
            tcp: &tcp,
            resolver: &ctx.resolver,
        };
        let mut found: Vec<(Origin, Option<f64>)> = Vec::new();
        match active_builds::read_dir(&ctx.paths.active_builds()) {
            Some(builds) => {
                found.extend(
                    builds
                        .iter()
                        .map(|b| (attr.for_active_build(b), b.start_time)),
                );
            }
            None => {
                // Upstream Nix: no active-builds/, find builds from processes.
                let cfg = ctx.nix_config()?;
                let uids = BuildUids::from_config(&cfg, &users);
                let boot = procs::boot_time(&ctx.paths.proc_dir).unwrap_or(0);
                for (nix_pid, top) in buildset::builds_from_processes(&procs, &uids) {
                    let start = procs.get(top).map(|p| procs::start_time_seconds(p, boot));
                    found.push((attr.for_nix_pid(nix_pid), start));
                }
            }
        }
        let mut counts: BTreeMap<Origin, f64> = BTreeMap::new();
        for (o, _) in &found {
            *counts.entry(o.clone()).or_default() += 1.0;
        }
        let mut active = Family::gauge(
            "nix_builds_active",
            "Builds running on this host, by who asked for them.",
        );
        for (o, n) in &counts {
            active.push(&[("origin", o.origin), ("client", &o.client)], *n);
        }
        if counts.is_empty() {
            // Keep the family present when idle, so queries need no `or vector(0)`.
            active.push(&[("origin", "local"), ("client", "")], 0.0);
        }
        let mut out = vec![active];
        if let Some(oldest) = found.iter().filter_map(|(_, t)| *t).reduce(f64::min) {
            out.push(
                Family::gauge(
                    "nix_build_oldest_start_timestamp_seconds",
                    "Start time of the longest-running active build.",
                )
                .with(&[], oldest),
            );
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::paths::Paths;
    use crate::testutil::{fake_proc, write};

    fn setup(root: &std::path::Path) -> Paths {
        let p = root.join("proc");
        fake_proc(&p, 1, 0, 0, "systemd", &["systemd"]);
        fake_proc(&p, 100, 1, 0, "nix-daemon", &["nix-daemon", "--daemon"]);
        fake_proc(&p, 150, 1, 1000, "nix", &["nix", "build"]);
        fake_proc(&p, 200, 100, 0, "nix-daemon", &["nix-daemon", "150"]);
        fake_proc(&p, 300, 200, 872415232, "bash", &["bash"]);
        write(&p.join("stat"), "btime 1792400000\n");
        write(
            &root.join("etc/passwd"),
            "root:x:0:0::/:/x\nburke:x:1000:100::/:/x\n",
        );
        Paths {
            proc_dir: p,
            state_dir: root.join("state"),
            etc_dir: root.join("etc"),
            ..Paths::default()
        }
    }

    const AUTO: &str = "auto-allocate-uids = true\nstart-id = 872415232\nid-count = 8388608\n";

    #[test]
    fn uses_active_builds_when_present() {
        let d = tempfile::tempdir().unwrap();
        let paths = setup(d.path());
        write(
            &paths.active_builds().join("200-1"),
            r#"{"nixPid":200,"mainPid":300,"startTime":1792482889.0,"clientPid":null,"clientUid":null,"cgroup":null,"type":"build"}"#,
        );
        let ctx = Ctx::for_test(paths, AUTO);
        let text = render(
            &Builds {
                interval: Duration::from_secs(1),
            }
            .collect(&ctx)
            .unwrap(),
        );
        assert!(
            text.contains("nix_builds_active{origin=\"local\",client=\"burke\"} 1\n"),
            "{text}"
        );
        assert!(text.contains("nix_build_oldest_start_timestamp_seconds 1792482889\n"));
    }

    #[test]
    fn falls_back_to_processes() {
        let d = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(setup(d.path()), AUTO);
        let text = render(
            &Builds {
                interval: Duration::from_secs(1),
            }
            .collect(&ctx)
            .unwrap(),
        );
        assert!(
            text.contains("nix_builds_active{origin=\"local\",client=\"burke\"} 1\n"),
            "{text}"
        );
        assert!(text.contains("nix_build_oldest_start_timestamp_seconds "));
    }

    #[test]
    fn zero_without_builds_or_uid_rule() {
        let d = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(setup(d.path()), "");
        let out = Builds {
            interval: Duration::from_secs(1),
        }
        .collect(&ctx)
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "nix_builds_active");
        let text = render(&out);
        assert!(
            text.contains("nix_builds_active{origin=\"local\",client=\"\"} 0\n"),
            "{text}"
        );
        assert!(!text.contains("nix_build_oldest_start_timestamp_seconds"));
    }
}
