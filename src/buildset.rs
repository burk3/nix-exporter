//! Which processes are builds.

use crate::nixconf::NixConfig;
use crate::procs::ProcTable;
use crate::users::Users;
use std::collections::HashSet;

/// The UIDs builds run as, from the daemon's settings: members of
/// `build-users-group`, and/or `[start-id, start-id + id-count)` with
/// `auto-allocate-uids`.
#[derive(Clone, Debug, Default)]
pub struct BuildUids {
    range: Option<(u32, u32)>,
    members: HashSet<u32>,
}

impl BuildUids {
    pub fn from_config(config: &NixConfig, users: &Users) -> Self {
        let range = config.flag("auto-allocate-uids").then(|| {
            let start = config.number("start-id").unwrap_or(872415232) as u32;
            let count = config.number("id-count").unwrap_or(8388608) as u32;
            (start, start.saturating_add(count))
        });
        let members = config
            .get("build-users-group")
            .map(|g| users.group_member_uids(g))
            .unwrap_or_default();
        BuildUids { range, members }
    }

    pub fn contains(&self, uid: u32) -> bool {
        self.members.contains(&uid) || self.range.is_some_and(|(s, e)| uid >= s && uid < e)
    }
}

/// PIDs of build processes: each active build's main process and its
/// descendants, plus every process running as a build UID (which also
/// catches processes that left the build's tree).
pub fn build_pids(t: &ProcTable, main_pids: &[u32], uids: &BuildUids) -> HashSet<u32> {
    let mut out: HashSet<u32> = main_pids.iter().flat_map(|p| t.subtree(*p)).collect();
    out.extend(
        t.procs
            .values()
            .filter(|p| uids.contains(p.uid))
            .map(|p| p.pid),
    );
    out
}

/// Builds found from processes, for Nix without `active-builds/`: each
/// build-UID process whose parent is not a build UID is the top of a build.
/// Returns `(parent pid, top pid)`; the parent is the build's `nixPid`.
pub fn builds_from_processes(t: &ProcTable, uids: &BuildUids) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = t
        .procs
        .values()
        .filter(|p| uids.contains(p.uid))
        .filter(|p| {
            t.get(p.ppid)
                .is_some_and(|parent| !uids.contains(parent.uid))
        })
        .map(|p| (p.ppid, p.pid))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::fake_proc;

    const AUTO: &str = "auto-allocate-uids = true\nstart-id = 872415232\nid-count = 8388608\nbuild-users-group = \n";
    const CLASSIC: &str = "auto-allocate-uids = false\nbuild-users-group = nixbld\n";
    const PASSWD: &str = "root:x:0:0::/:/x\nnixbld1:x:30001:30000::/:/x\n";
    const GROUP: &str = "nixbld:x:30000:nixbld1\n";

    fn tree() -> (tempfile::TempDir, ProcTable) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fake_proc(p, 1, 0, 0, "systemd", &["systemd"]);
        fake_proc(p, 100, 1, 0, "nix-daemon", &["nix-daemon", "--daemon"]);
        fake_proc(p, 200, 100, 0, "nix-daemon", &["nix-daemon", "4242"]);
        fake_proc(
            p,
            300,
            200,
            872415232,
            "bash",
            &["bash", "-e", "builder.sh"],
        );
        fake_proc(p, 301, 300, 872415232, "make", &["make"]);
        fake_proc(p, 400, 1, 872415300, "daemonized", &["x"]);
        fake_proc(p, 500, 200, 30001, "bash", &["bash"]);
        let t = ProcTable::read(p);
        (d, t)
    }

    #[test]
    fn auto_allocated_uids() {
        let (_d, t) = tree();
        let uids = BuildUids::from_config(&NixConfig::parse(AUTO), &Users::parse(PASSWD, GROUP));
        assert!(uids.contains(872415232));
        assert!(!uids.contains(30001));
        assert_eq!(builds_from_processes(&t, &uids), vec![(1, 400), (200, 300)]);
        let mut pids: Vec<u32> = build_pids(&t, &[], &uids).into_iter().collect();
        pids.sort();
        assert_eq!(pids, vec![300, 301, 400]);
    }

    #[test]
    fn classic_build_users() {
        let (_d, t) = tree();
        let uids = BuildUids::from_config(&NixConfig::parse(CLASSIC), &Users::parse(PASSWD, GROUP));
        assert!(uids.contains(30001));
        assert!(!uids.contains(872415232));
        assert_eq!(builds_from_processes(&t, &uids), vec![(200, 500)]);
    }

    #[test]
    fn main_pid_subtrees_count_without_uid_rule() {
        let (_d, t) = tree();
        let none = BuildUids::default();
        let mut pids: Vec<u32> = build_pids(&t, &[300], &none).into_iter().collect();
        pids.sort();
        assert_eq!(pids, vec![300, 301]);
        assert!(builds_from_processes(&t, &none).is_empty());
    }
}
