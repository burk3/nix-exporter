use crate::collector::{Collector, Ctx};
use crate::locks;
use crate::machines;
use crate::metrics::Family;
use anyhow::Result;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

pub struct Slots {
    pub interval: Duration,
}

impl Collector for Slots {
    fn name(&self) -> &'static str {
        "slots"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let cfg = ctx.nix_config()?;
        let machines = machines::load(cfg.get("builders"), &ctx.extra_machines);
        let held = locks::held_files(
            &fs::read_to_string(ctx.paths.proc_dir.join("locks")).unwrap_or_default(),
        );
        let dir = ctx.paths.current_load();
        // /proc/locks names the superblock's device, which on btrfs is not
        // stat's st_dev; take it from the mount when we can.
        let lock_dev = fs::canonicalize(&dir).ok().and_then(|d| {
            let info =
                fs::read_to_string(ctx.paths.proc_dir.join("self/mountinfo")).unwrap_or_default();
            locks::mount_dev(&info, &d)
        });
        let files: Vec<(String, (u64, u64))> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                let dev = lock_dev.unwrap_or(md.dev());
                Some((e.file_name().into_string().ok()?, (dev, md.ino())))
            })
            .collect();
        let mut slots = Family::gauge(
            "nix_builder_slots",
            "Build slots (maxJobs) of each remote builder.",
        );
        let mut used = Family::gauge(
            "nix_builds_offloaded",
            "Builds this host is running on each remote builder.",
        );
        for m in machines.iter().filter(|m| !m.is_local()) {
            let prefix = m.slot_prefix();
            let n = files
                .iter()
                .filter(|(name, id)| {
                    name.strip_prefix(&prefix).is_some_and(|rest| {
                        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
                    }) && held.contains(id)
                })
                .count();
            slots.push(&[("machine", &m.uri)], m.max_jobs as f64);
            used.push(&[("machine", &m.uri)], n as f64);
        }
        Ok(vec![slots, used])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::paths::Paths;
    use crate::testutil::write;

    #[test]
    fn counts_held_slots() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let paths = Paths {
            proc_dir: root.join("proc"),
            state_dir: root.join("state"),
            ..Paths::default()
        };
        write(
            &root.join("machines"),
            "ssh-ng://remotebuild@juicy-j x86_64-linux /k 32 4\nlocalhost x86_64-linux - 16 1\n",
        );
        let load = paths.current_load();
        for f in [
            "ssh-ng:__remotebuild@juicy-j-0",
            "ssh-ng:__remotebuild@juicy-j-1",
            "ssh-ng:__remotebuild@juicy-j.upload-lock",
            "main-lock",
        ] {
            write(&load.join(f), "");
        }
        let id = |f: &str| {
            let md = fs::metadata(load.join(f)).unwrap();
            format!(
                "{:02x}:{:02x}:{}",
                libc::major(md.dev()),
                libc::minor(md.dev()),
                md.ino()
            )
        };
        // Same inode as juicy-j-0 on another device: some other file.
        let other_dev = {
            let md = fs::metadata(load.join("ssh-ng:__remotebuild@juicy-j-0")).unwrap();
            format!(
                "{:02x}:{:02x}:{}",
                libc::major(md.dev()),
                libc::minor(md.dev()) + 1,
                md.ino()
            )
        };
        write(
            &root.join("proc/locks"),
            &format!(
                "1: FLOCK  ADVISORY  WRITE 819892 {} 0 EOF\n2: FLOCK  ADVISORY  WRITE 1 {} 0 EOF\n3: FLOCK  ADVISORY  WRITE 2 {} 0 EOF\n",
                id("ssh-ng:__remotebuild@juicy-j-1"),
                id("ssh-ng:__remotebuild@juicy-j.upload-lock"),
                other_dev
            ),
        );
        let ctx = Ctx::for_test(
            paths,
            &format!("builders = @{}\n", root.join("machines").display()),
        );
        let text = render(
            &Slots {
                interval: Duration::from_secs(1),
            }
            .collect(&ctx)
            .unwrap(),
        );
        assert!(
            text.contains("nix_builder_slots{machine=\"ssh-ng://remotebuild@juicy-j\"} 32\n"),
            "{text}"
        );
        assert!(text.contains("nix_builds_offloaded{machine=\"ssh-ng://remotebuild@juicy-j\"} 1\n"));
        assert!(!text.contains("localhost"));
    }

    /// On btrfs `stat` reports a subvolume device, while /proc/locks uses the
    /// superblock's, as mountinfo does.
    #[test]
    fn takes_the_lock_device_from_mountinfo() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let paths = Paths {
            proc_dir: root.join("proc"),
            state_dir: root.join("state"),
            ..Paths::default()
        };
        write(&root.join("machines"), "ssh-ng://b x86_64-linux - 4 1\n");
        let load = paths.current_load();
        for f in ["ssh-ng:__b-0", "ssh-ng:__b-1"] {
            write(&load.join(f), "");
        }
        let md = |f: &str| fs::metadata(load.join(f)).unwrap();
        write(
            &root.join("proc/self/mountinfo"),
            "22 1 0:99 / / rw shared:1 - btrfs /dev/x rw\n",
        );
        write(
            &root.join("proc/locks"),
            &format!(
                "1: FLOCK  ADVISORY  WRITE 1 00:63:{} 0 EOF\n2: FLOCK  ADVISORY  WRITE 2 00:63:{} 0 EOF\n",
                md("ssh-ng:__b-0").ino(),
                md("ssh-ng:__b-1").ino(),
            ),
        );
        let ctx = Ctx::for_test(
            paths,
            &format!("builders = @{}\n", root.join("machines").display()),
        );
        let text = render(
            &Slots {
                interval: Duration::from_secs(1),
            }
            .collect(&ctx)
            .unwrap(),
        );
        assert!(
            text.contains("nix_builds_offloaded{machine=\"ssh-ng://b\"} 2\n"),
            "{text}"
        );
    }
}
