//! Who asked for a build: `origin` (`local`, `hydra`, `remote`, `unknown`) and `client`.

use crate::active_builds::ActiveBuild;
use crate::nettcp::TcpTable;
use crate::procs::ProcTable;
use crate::resolve::Resolver;
use crate::users::Users;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    pub origin: &'static str,
    pub client: String,
}

impl Origin {
    fn new(origin: &'static str, client: impl Into<String>) -> Self {
        Origin {
            origin,
            client: client.into(),
        }
    }

    pub fn unknown() -> Self {
        Self::new("unknown", "")
    }
}

pub struct Attributor<'a> {
    pub procs: &'a ProcTable,
    pub users: &'a Users,
    pub tcp: &'a TcpTable,
    pub resolver: &'a Resolver,
}

impl Attributor<'_> {
    pub fn for_active_build(&self, b: &ActiveBuild) -> Origin {
        if let Some(pid) = b.client_pid {
            let origin = self.for_client(pid);
            if origin != Origin::unknown() {
                return origin;
            }
            if let Some(uid) = b.client_uid {
                return Origin::new("local", self.users.name(uid));
            }
            return origin;
        }
        if let Some(uid) = b.client_uid {
            return Origin::new("local", self.users.name(uid));
        }
        self.for_nix_pid(b.nix_pid)
    }

    /// `nix_pid` started the build: a daemon connection worker, or a `nix*`
    /// process building in-process (root without the daemon).
    pub fn for_nix_pid(&self, nix_pid: u32) -> Origin {
        let Some(p) = self.procs.get(nix_pid) else {
            return Origin::unknown();
        };
        if p.comm == "nix-daemon" {
            return match p.worker_client_pid() {
                Some(client) => self.for_client(client),
                None => Origin::unknown(),
            };
        }
        if p.comm.starts_with("nix") {
            return Origin::new("local", self.users.name(p.uid));
        }
        Origin::unknown()
    }

    pub fn for_client(&self, pid: u32) -> Origin {
        let Some(c) = self.procs.get(pid) else {
            return Origin::unknown();
        };
        if c.is_remote_server() {
            // Without an sshd above it (e.g. Hydra spawning `nix-store --serve`
            // locally) it is not remote; fall through.
            if let Some(peer) = self.peer_of(pid) {
                return Origin::new("remote", peer);
            }
        }
        let hydra = self
            .procs
            .ancestors(pid)
            .iter()
            .any(|a| self.users.name(a.uid) == "hydra-queue-runner");
        if hydra {
            return Origin::new("hydra", "hydra");
        }
        Origin::new("local", self.users.name(c.uid))
    }

    /// The ssh client address of the sshd session above `pid`, reverse-resolved.
    pub fn peer_of(&self, pid: u32) -> Option<String> {
        self.procs
            .ancestors(pid)
            .iter()
            .filter(|a| a.comm.starts_with("sshd"))
            .flat_map(|a| self.procs.socket_inodes(a.pid))
            .find_map(|ino| self.tcp.remote(ino))
            .map(|ip| self.resolver.name(ip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fake_proc, fake_socket_fd};

    const PASSWD: &str = "root:x:0:0::/:/x\nburke:x:1000:100::/:/x\nremotebuild:x:1001:100::/:/x\nhydra-queue-runner:x:2000:2000::/:/x\n";
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0EA57364:0016 37924A64:B354 01 00000000:00000000 02:000A3B2C 00000000     0        0 11035730 2 0000000000000000 20 4 30 10 -1\n";

    /// The process chains seen on juicy-j with research/probe.sh.
    fn setup() -> (tempfile::TempDir, ProcTable, Users, TcpTable) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fake_proc(
            p,
            823418,
            1,
            0,
            "nix-daemon",
            &["nix-daemon", "--option", "json-log-path", "/x"],
        );
        // remote ssh-ng client
        fake_proc(
            p,
            1686,
            1,
            0,
            "sshd",
            &["sshd: /nix/store/x/bin/sshd -D [listener]"],
        );
        fake_proc(
            p,
            1320157,
            1686,
            0,
            "sshd-session",
            &["sshd-session: remotebuild [postauth]"],
        );
        fake_proc(
            p,
            1320160,
            1320157,
            1001,
            "sshd-session",
            &["sshd-session: remotebuild@notty"],
        );
        fake_proc(
            p,
            1320161,
            1320160,
            1001,
            "nix-daemon",
            &["nix-daemon", "--stdio"],
        );
        fake_proc(
            p,
            1320186,
            823418,
            0,
            "nix-daemon",
            &["nix-daemon", "1320161", "json-log-path", "/x"],
        );
        fake_socket_fd(p, 1320160, 7, 11035730);
        // local client
        fake_proc(p, 1296115, 1, 1000, "nix", &["nix", "build"]);
        fake_proc(
            p,
            1296284,
            823418,
            0,
            "nix-daemon",
            &["nix-daemon", "1296115", "json-log-path", "/x"],
        );
        // hydra
        fake_proc(
            p,
            401264,
            1,
            2000,
            "hydra-queue-run",
            &["hydra-queue-runner", "-v"],
        );
        fake_proc(
            p,
            401300,
            823418,
            0,
            "nix-daemon",
            &["nix-daemon", "401264", "json-log-path", "/x"],
        );
        // root building in-process
        fake_proc(p, 1296095, 1, 0, "nix", &["nix", "build"]);
        // worker whose client is gone
        fake_proc(
            p,
            1400000,
            823418,
            0,
            "nix-daemon",
            &["nix-daemon", "999999", "json-log-path", "/x"],
        );
        let mut tcp = TcpTable::default();
        tcp.add(TCP);
        let t = ProcTable::read(p);
        (d, t, Users::parse(PASSWD, ""), tcp)
    }

    #[test]
    fn attributes_each_kind_of_client() {
        let (_d, procs, users, tcp) = setup();
        let resolver = Resolver::new(false);
        let a = Attributor {
            procs: &procs,
            users: &users,
            tcp: &tcp,
            resolver: &resolver,
        };
        assert_eq!(
            a.for_nix_pid(1320186),
            Origin::new("remote", "100.74.146.55")
        );
        assert_eq!(a.for_nix_pid(1296284), Origin::new("local", "burke"));
        assert_eq!(a.for_nix_pid(401300), Origin::new("hydra", "hydra"));
        assert_eq!(a.for_nix_pid(1296095), Origin::new("local", "root"));
        assert_eq!(a.for_nix_pid(1400000), Origin::unknown());
        assert_eq!(a.for_nix_pid(42), Origin::unknown());
        assert_eq!(a.peer_of(1320161).as_deref(), Some("100.74.146.55"));
    }

    #[test]
    fn active_build_client_fields_take_precedence() {
        let (_d, procs, users, tcp) = setup();
        let resolver = Resolver::new(false);
        let a = Attributor {
            procs: &procs,
            users: &users,
            tcp: &tcp,
            resolver: &resolver,
        };
        let mut b = ActiveBuild {
            nix_pid: 1296284,
            main_pid: None,
            start_time: None,
            client_pid: None,
            client_uid: None,
            cgroup: None,
        };
        assert_eq!(a.for_active_build(&b), Origin::new("local", "burke"));
        b.client_pid = Some(1320161);
        assert_eq!(
            a.for_active_build(&b),
            Origin::new("remote", "100.74.146.55")
        );
        b.client_pid = None;
        b.client_uid = Some(1001);
        assert_eq!(a.for_active_build(&b), Origin::new("local", "remotebuild"));
        b.client_pid = Some(999999);
        b.client_uid = Some(1000);
        assert_eq!(a.for_active_build(&b), Origin::new("local", "burke"));
    }
}
