//! A snapshot of `/proc`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    pub comm: String,
    pub argv: Vec<String>,
    pub rss_bytes: u64,
    pub start_ticks: u64,
}

impl Proc {
    /// For a `nix-daemon` connection worker, the client PID. Nix writes it over
    /// argv[1], NUL-padded to the original argument's length.
    pub fn worker_client_pid(&self) -> Option<u32> {
        if self.comm != "nix-daemon" {
            return None;
        }
        self.argv.get(1)?.trim_end_matches('\0').parse().ok()
    }

    /// A process serving a remote Nix client over ssh: `nix-daemon --stdio`
    /// (ssh-ng) or `nix-store --serve` (legacy ssh).
    pub fn is_remote_server(&self) -> bool {
        let has = |a: &str| self.argv.iter().any(|x| x == a);
        (self.comm == "nix-daemon" && has("--stdio"))
            || (self.comm == "nix-store" && has("--serve"))
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProcTable {
    pub procs: HashMap<u32, Proc>,
    proc_dir: PathBuf,
}

impl ProcTable {
    pub fn read(proc_dir: &Path) -> Self {
        let mut procs = HashMap::new();
        if let Ok(entries) = fs::read_dir(proc_dir) {
            for e in entries.flatten() {
                let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                    continue;
                };
                if let Some(p) = read_proc(&e.path(), pid) {
                    procs.insert(pid, p);
                }
            }
        }
        ProcTable {
            procs,
            proc_dir: proc_dir.to_path_buf(),
        }
    }

    pub fn get(&self, pid: u32) -> Option<&Proc> {
        self.procs.get(&pid)
    }

    /// `pid` and its ancestors, nearest first.
    pub fn ancestors(&self, pid: u32) -> Vec<&Proc> {
        let mut out = Vec::new();
        let mut cur = self.get(pid);
        while let Some(p) = cur {
            if out.len() > 64 {
                break;
            }
            out.push(p);
            if p.ppid == 0 || p.ppid == p.pid {
                break;
            }
            cur = self.get(p.ppid);
        }
        out
    }

    pub fn children_of(&self, pid: u32) -> impl Iterator<Item = &Proc> {
        self.procs.values().filter(move |p| p.ppid == pid)
    }

    /// `pid` and all of its descendants.
    pub fn subtree(&self, pid: u32) -> Vec<u32> {
        let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in self.procs.values() {
            kids.entry(p.ppid).or_default().push(p.pid);
        }
        let mut seen = HashSet::new();
        let mut stack = vec![pid];
        while let Some(p) = stack.pop() {
            if !self.procs.contains_key(&p) || !seen.insert(p) {
                continue;
            }
            if let Some(k) = kids.get(&p) {
                stack.extend(k);
            }
        }
        seen.into_iter().collect()
    }

    /// Inodes of the sockets `pid` holds open.
    pub fn socket_inodes(&self, pid: u32) -> Vec<u64> {
        let Ok(entries) = fs::read_dir(self.proc_dir.join(pid.to_string()).join("fd")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| fs::read_link(e.path()).ok())
            .filter_map(|t| {
                t.to_str()?
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .parse()
                    .ok()
            })
            .collect()
    }
}

fn read_proc(dir: &Path, pid: u32) -> Option<Proc> {
    let status = fs::read_to_string(dir.join("status")).ok()?;
    let (mut comm, mut ppid, mut uid, mut rss_kb) = (String::new(), 0, None, 0u64);
    for line in status.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k {
            "Name" => comm = v.to_string(),
            "PPid" => ppid = v.parse().unwrap_or(0),
            "Uid" => uid = v.split_whitespace().next().and_then(|n| n.parse().ok()),
            "VmRSS" => {
                rss_kb = v
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            }
            _ => {}
        }
    }
    let cmdline = fs::read(dir.join("cmdline")).unwrap_or_default();
    let mut argv: Vec<String> = cmdline
        .split(|b| *b == 0)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    if argv.last().is_some_and(|s| s.is_empty()) {
        argv.pop();
    }
    let stat = fs::read_to_string(dir.join("stat")).unwrap_or_default();
    // Fields after the ")" that ends comm start at field 3 (state); starttime is field 22.
    let start_ticks = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some(Proc {
        pid,
        ppid,
        uid: uid?,
        comm,
        argv,
        rss_bytes: rss_kb * 1024,
        start_ticks,
    })
}

/// Boot time in seconds since the epoch, from `/proc/stat`.
pub fn boot_time(proc_dir: &Path) -> Option<u64> {
    fs::read_to_string(proc_dir.join("stat"))
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime ")?.trim().parse().ok())
}

fn clock_ticks() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if t > 0 {
        t as u64
    } else {
        100
    }
}

pub fn start_time_seconds(p: &Proc, boot: u64) -> f64 {
    boot as f64 + p.start_ticks as f64 / clock_ticks() as f64
}

pub struct DaemonProcs {
    pub main: Option<u32>,
    pub workers: Vec<u32>,
}

/// The main Nix daemon (a `nix-daemon` that is neither a connection worker nor
/// an ssh `--stdio` server, and whose parent is not a `nix-daemon`) and its
/// worker children.
pub fn find_daemon(t: &ProcTable) -> DaemonProcs {
    let main = t
        .procs
        .values()
        .filter(|p| {
            p.comm == "nix-daemon" && p.worker_client_pid().is_none() && !p.is_remote_server()
        })
        .filter(|p| t.get(p.ppid).is_none_or(|pp| pp.comm != "nix-daemon"))
        .min_by_key(|p| (p.start_ticks, p.pid))
        .map(|p| p.pid);
    let workers = match main {
        Some(m) => t
            .children_of(m)
            .filter(|p| p.comm == "nix-daemon")
            .map(|p| p.pid)
            .collect(),
        None => Vec::new(),
    };
    DaemonProcs { main, workers }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fake_proc, fake_socket_fd, write};

    /// determinate-nixd (90) -> nix-daemon (100) -> worker (200, client 4242)
    /// sshd (290) -> nix-daemon --stdio (300), nix-store --serve (400)
    fn tree() -> (tempfile::TempDir, ProcTable) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        fake_proc(
            p,
            90,
            1,
            0,
            "determinate-nix",
            &["determinate-nixd", "daemon"],
        );
        fake_proc(
            p,
            100,
            90,
            0,
            "nix-daemon",
            &["nix-daemon", "--option", "json-log-path", "/x"],
        );
        fake_proc(
            p,
            200,
            100,
            0,
            "nix-daemon",
            &["nix-daemon", "4242\0", "json-log-path", "/x"],
        );
        fake_proc(p, 210, 100, 0, "crashpad_handle", &["crashpad_handler"]);
        fake_proc(
            p,
            290,
            1,
            1001,
            "sshd-session",
            &["sshd-session: remotebuild@notty"],
        );
        fake_proc(p, 300, 290, 1001, "nix-daemon", &["nix-daemon", "--stdio"]);
        fake_proc(
            p,
            400,
            290,
            1001,
            "nix-store",
            &["nix-store", "--serve", "--write"],
        );
        fake_socket_fd(p, 290, 7, 11035730);
        write(&p.join("stat"), "cpu 1 2 3\nbtime 1792400000\n");
        let t = ProcTable::read(p);
        (d, t)
    }

    #[test]
    fn reads_processes() {
        let (_d, t) = tree();
        let w = t.get(200).unwrap();
        assert_eq!(w.ppid, 100);
        assert_eq!(w.uid, 0);
        assert_eq!(w.rss_bytes, 1024000);
        assert_eq!(w.start_ticks, 5000);
        assert_eq!(w.argv[0], "nix-daemon");
    }

    #[test]
    fn worker_titles_and_remote_servers() {
        let (_d, t) = tree();
        assert_eq!(t.get(100).unwrap().worker_client_pid(), None);
        assert_eq!(t.get(200).unwrap().worker_client_pid(), Some(4242));
        assert_eq!(t.get(300).unwrap().worker_client_pid(), None);
        assert!(t.get(300).unwrap().is_remote_server());
        assert!(t.get(400).unwrap().is_remote_server());
        assert!(!t.get(100).unwrap().is_remote_server());
    }

    #[test]
    fn finds_daemon_and_workers() {
        let (_d, t) = tree();
        let d = find_daemon(&t);
        assert_eq!(d.main, Some(100));
        assert_eq!(d.workers, vec![200]);
    }

    #[test]
    fn ancestry_subtree_and_sockets() {
        let (d, t) = tree();
        let chain: Vec<u32> = t.ancestors(200).iter().map(|p| p.pid).collect();
        assert_eq!(chain, vec![200, 100, 90]);
        let mut sub = t.subtree(100);
        sub.sort();
        assert_eq!(sub, vec![100, 200, 210]);
        assert_eq!(t.socket_inodes(290), vec![11035730]);
        assert!(t.socket_inodes(300).is_empty());
        assert_eq!(boot_time(d.path()), Some(1792400000));
    }
}
