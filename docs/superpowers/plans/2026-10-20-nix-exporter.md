# nix-exporter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `nix-exporter`, a Rust Prometheus exporter for the Nix store and daemon, packaged as a flake with NixOS and system-manager modules and a NixOS test, then deploy it to juicy-j on an all-the-nix branch.

**Architecture:** One binary with three subcommands. `collect` (root, no network listener) runs one thread per collector and atomically rewrites `/run/nix-exporter/metrics.prom` after every cycle. `serve` (unprivileged) serves that file on `/metrics`. `print-dead` prints the dead-path set for testing. Everything that reads system state is a small module with a pure, unit-tested parser; collectors glue parsers together.

**Tech Stack:** Rust 2021 (anyhow, clap, rusqlite with bundled SQLite, serde/serde_json, tiny_http, dns-lookup, libc; tempfile for tests), Nix flake with crane, NixOS test driver (QEMU and systemd-nspawn backends), system-manager.

**Spec:** `docs/superpowers/specs/2026-10-20-nix-exporter-design.md`. Read it before starting; this plan implements it.

---

## Conventions for every task

- Work from `/home/burke/src/nix-exporter` on branch `master`.
- Enter the dev shell for cargo: prefix commands with `nix develop -c` (e.g. `nix develop -c cargo test`). Task 1 creates the shell.
- After writing code: `nix develop -c cargo fmt` then `nix develop -c cargo clippy --all-targets -- -D warnings` must be clean before committing.
- Unit tests live in a `#[cfg(test)] mod tests` at the bottom of the module they test. Fixtures are inline string constants (crane's `cleanCargoSource` drops non-Rust files, so no fixture files).
- New modules are declared in `src/main.rs` as they are created (each task says which `mod` line to add).
- Commit messages end with:

  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  ```
- Until Task 13 wires everything into `main`, many items are unused; add `#![allow(dead_code)]` at the top of `src/main.rs` in Task 1 and remove it in Task 13.

## File map

| file | responsibility |
|---|---|
| `Cargo.toml` | crate manifest |
| `src/main.rs` | CLI (`collect`, `serve`, `print-dead`), module declarations |
| `src/metrics.rs` | `Family`/`Sample` types and the text exposition encoder |
| `src/cmd.rs` | subprocesses with timeouts |
| `src/paths.rs` | `/proc`, `/sys/fs/cgroup`, `/nix/var/nix`, `/etc` locations (overridable in tests) |
| `src/nixconf.rs` | `nix config show` parsing |
| `src/users.rs` | `/etc/passwd`, `/etc/group` |
| `src/procs.rs` | process table from `/proc`, worker titles, daemon discovery |
| `src/nettcp.rs` | `/proc/net/tcp{,6}` inode → remote address |
| `src/resolve.rs` | cached reverse DNS |
| `src/machines.rs` | machines files, slot file names, ping URIs |
| `src/locks.rs` | `/proc/locks` |
| `src/active_builds.rs` | Determinate's `active-builds/` JSON |
| `src/buildset.rs` | build UIDs, build processes, process-based build discovery |
| `src/attribution.rs` | who asked for a build (`origin`, `client`) |
| `src/cgroup.rs` | daemon unit cgroup v2 metrics |
| `src/upkeep_log.rs` | `systemctl show` and GC/optimise log parsing |
| `src/storedb.rs` | read-only store DB queries |
| `src/liveness.rs` | reference graph, dead-path walk, roots parsing |
| `src/profiles.rs` | profile generation link parsing |
| `src/collector.rs` | `Collector` trait, `Ctx`, `Snapshot` file writer, thread runner |
| `src/collectors/*.rs` | one file per collector: `store`, `liveness`, `profiles`, `daemon`, `builds`, `slots`, `builders`, `upkeep` |
| `src/serve.rs` | HTTP `/metrics` from the snapshot file |
| `src/testutil.rs` | test helpers: fake `/proc` trees |
| `nix/package.nix` | crane build |
| `nix/module.nix` | shared module: options and both systemd units |
| `nix/nixos.nix` | NixOS module: core + firewall + `nix.package` default |
| `nix/system-manager.nix` | system-manager module: core only |
| `nix/test.nix` | NixOS test, `backend = "vm"` or `"container"` |
| `flake.nix` | inputs (nixpkgs, crane, system-manager), outputs |
| `README.md` | usage, metrics, caveats |

---

### Task 1: Scaffold the crate and dev shell

**Files:**
- Create: `Cargo.toml`, `src/main.rs`, `.gitignore`
- Modify: `flake.nix` (whole file; it is the user's template, currently staged but uncommitted)

- [ ] **Step 1: Write `Cargo.toml`**

```toml
[package]
name = "nix-exporter"
version = "0.1.0"
edition = "2021"
description = "Prometheus exporter for the Nix store and daemon"

[dependencies]
anyhow = "1"
clap = { version = "4", features = ["derive"] }
dns-lookup = "4"
libc = "0.2"
rusqlite = { version = "0.40", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tiny_http = "0.12"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Write `src/main.rs` placeholder**

```rust
#![allow(dead_code)]

fn main() {
    println!("nix-exporter");
}
```

- [ ] **Step 3: Write `.gitignore`**

```
/target
/result*
.direnv
```

- [ ] **Step 4: Replace `flake.nix`** (later tasks add outputs; this version only adds inputs and the Rust dev shell)

```nix
{
  description = "Prometheus exporter for the Nix store and daemon";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0";
    crane.url = "github:ipetkov/crane";
    system-manager = {
      url = "github:numtide/system-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, ... }@inputs:
    let
      inherit (inputs.nixpkgs) lib;

      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      linuxSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      forSystems =
        systems: f:
        lib.genAttrs systems (
          system:
          f {
            inherit system;
            pkgs = import inputs.nixpkgs {
              inherit system;
              config.allowUnfree = true;
            };
          }
        );
      forEachSupportedSystem = forSystems supportedSystems;
      forEachLinuxSystem = forSystems linuxSystems;
    in
    {
      devShells = forEachSupportedSystem (
        { pkgs, system }:
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              self.formatter.${system}
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
            ];
          };
        }
      );

      formatter = forEachSupportedSystem ({ pkgs, ... }: pkgs.nixfmt);
    };
}
```

`forEachLinuxSystem` is unused until Task 14; Nix does not warn about unused bindings.

- [ ] **Step 5: Lock and build**

Run: `nix flake lock && nix develop -c cargo build`
Expected: `flake.lock` created; cargo downloads crates, writes `Cargo.lock`, and builds; `nix develop -c cargo run` prints `nix-exporter`.

- [ ] **Step 6: Commit** (this also commits the user's previously staged `.envrc`)

```bash
git add Cargo.toml Cargo.lock src/main.rs .gitignore flake.nix flake.lock .envrc
git commit -m "Scaffold nix-exporter crate and Rust dev shell"
```

---

### Task 2: Exposition format encoder

**Files:**
- Create: `src/metrics.rs`
- Modify: `src/main.rs` (add `mod metrics;` above `fn main`)

- [ ] **Step 1: Write `src/metrics.rs` with tests first**

```rust
//! Prometheus text exposition format (version 0.0.4).

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Gauge,
    Counter,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    pub name: String,
    pub help: String,
    pub kind: Kind,
    pub samples: Vec<Sample>,
}

impl Family {
    pub fn gauge(name: &str, help: &str) -> Self {
        Self::new(name, help, Kind::Gauge)
    }

    pub fn counter(name: &str, help: &str) -> Self {
        Self::new(name, help, Kind::Counter)
    }

    fn new(name: &str, help: &str, kind: Kind) -> Self {
        Family {
            name: name.to_string(),
            help: help.to_string(),
            kind,
            samples: Vec::new(),
        }
    }

    pub fn push(&mut self, labels: &[(&str, &str)], value: f64) {
        self.samples.push(Sample {
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            value,
        });
    }

    pub fn with(mut self, labels: &[(&str, &str)], value: f64) -> Self {
        self.push(labels, value);
        self
    }
}

fn escape_label(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

fn escape_help(v: &str) -> String {
    v.replace('\\', "\\\\").replace('\n', "\\n")
}

fn format_value(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v > 0.0 { "+Inf" } else { "-Inf" }.to_string()
    } else if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Renders families in order. Families without samples are skipped.
pub fn render(families: &[Family]) -> String {
    let mut out = String::new();
    for f in families.iter().filter(|f| !f.samples.is_empty()) {
        let kind = match f.kind {
            Kind::Gauge => "gauge",
            Kind::Counter => "counter",
        };
        let _ = writeln!(out, "# HELP {} {}", f.name, escape_help(&f.help));
        let _ = writeln!(out, "# TYPE {} {}", f.name, kind);
        for s in &f.samples {
            out.push_str(&f.name);
            if !s.labels.is_empty() {
                out.push('{');
                for (i, (k, v)) in s.labels.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    let _ = write!(out, "{k}=\"{}\"", escape_label(v));
                }
                out.push('}');
            }
            let _ = writeln!(out, " {}", format_value(s.value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_gauge_with_labels() {
        let f = Family::gauge("nix_x", "An x.").with(&[("a", "1"), ("b", "two")], 3.0);
        assert_eq!(
            render(&[f]),
            "# HELP nix_x An x.\n# TYPE nix_x gauge\nnix_x{a=\"1\",b=\"two\"} 3\n"
        );
    }

    #[test]
    fn renders_counter_without_labels_and_fraction() {
        let f = Family::counter("nix_y_total", "A y.").with(&[], 1.5);
        assert_eq!(
            render(&[f]),
            "# HELP nix_y_total A y.\n# TYPE nix_y_total counter\nnix_y_total 1.5\n"
        );
    }

    #[test]
    fn escapes_label_values_and_help() {
        let f = Family::gauge("nix_z", "line\\one\ntwo").with(&[("l", "a\"b\\c\nd")], 0.0);
        assert_eq!(
            render(&[f]),
            "# HELP nix_z line\\\\one\\ntwo\n# TYPE nix_z gauge\nnix_z{l=\"a\\\"b\\\\c\\nd\"} 0\n"
        );
    }

    #[test]
    fn skips_empty_families() {
        assert_eq!(render(&[Family::gauge("nix_empty", "Nothing.")]), "");
    }
}
```

- [ ] **Step 2: Add `mod metrics;` to `src/main.rs`**, then run: `nix develop -c cargo test metrics`
Expected: 4 tests pass.

- [ ] **Step 3: fmt, clippy, commit**

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/metrics.rs src/main.rs
git commit -m "Add Prometheus text exposition encoder"
```

---

### Task 3: Subprocesses, paths, Nix config, users

**Files:**
- Create: `src/cmd.rs`, `src/paths.rs`, `src/nixconf.rs`, `src/users.rs`
- Modify: `src/main.rs` (add `mod cmd; mod nixconf; mod paths; mod users;`)

- [ ] **Step 1: Write `src/cmd.rs`**

```rust
//! Running subprocesses with a timeout.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn wait(child: &mut Child, desc: &str, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{desc} timed out after {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `cmd` and returns its stdout. Fails on timeout (the child is killed)
/// or a non-zero exit, with stderr in the error.
pub fn output(cmd: &mut Command, timeout: Duration) -> Result<String> {
    let desc = format!("{cmd:?}");
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {desc}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let status = wait(&mut child, &desc, timeout)?;
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if !status.success() {
        bail!("{desc} failed with {status}: {}", err.trim());
    }
    Ok(out)
}

/// Runs `cmd` with its output discarded; succeeds only on a zero exit within `timeout`.
/// Used where the child may leave grandchildren (ssh) holding pipes open.
pub fn status(cmd: &mut Command, timeout: Duration) -> Result<()> {
    let desc = format!("{cmd:?}");
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {desc}"))?;
    let status = wait(&mut child, &desc, timeout)?;
    if !status.success() {
        bail!("{desc} failed with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn returns_stdout() {
        assert_eq!(output(&mut sh("echo hi"), Duration::from_secs(5)).unwrap(), "hi\n");
    }

    #[test]
    fn fails_with_stderr_on_nonzero_exit() {
        let e = output(&mut sh("echo oops >&2; exit 3"), Duration::from_secs(5)).unwrap_err();
        assert!(format!("{e:#}").contains("oops"));
    }

    #[test]
    fn kills_on_timeout() {
        let start = Instant::now();
        let e = status(&mut sh("sleep 5"), Duration::from_millis(200)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(format!("{e}").contains("timed out"));
    }

    #[test]
    fn status_reports_failure() {
        assert!(status(&mut sh("exit 0"), Duration::from_secs(5)).is_ok());
        assert!(status(&mut sh("exit 1"), Duration::from_secs(5)).is_err());
    }
}
```

- [ ] **Step 2: Write `src/paths.rs`**

```rust
//! Filesystem locations, overridable so tests can point at fixture trees.

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Paths {
    pub proc_dir: PathBuf,
    pub cgroup_root: PathBuf,
    pub state_dir: PathBuf,
    pub etc_dir: PathBuf,
    pub store_dir: String,
}

impl Default for Paths {
    fn default() -> Self {
        Paths {
            proc_dir: "/proc".into(),
            cgroup_root: "/sys/fs/cgroup".into(),
            state_dir: "/nix/var/nix".into(),
            etc_dir: "/etc".into(),
            store_dir: "/nix/store".into(),
        }
    }
}

impl Paths {
    pub fn db(&self) -> PathBuf {
        self.state_dir.join("db/db.sqlite")
    }

    pub fn active_builds(&self) -> PathBuf {
        self.state_dir.join("active-builds")
    }

    pub fn current_load(&self) -> PathBuf {
        self.state_dir.join("current-load")
    }

    pub fn temproots(&self) -> PathBuf {
        self.state_dir.join("temproots")
    }

    pub fn gcroots_auto(&self) -> PathBuf {
        self.state_dir.join("gcroots/auto")
    }
}
```

- [ ] **Step 3: Write `src/nixconf.rs`**

```rust
//! The daemon's settings, from `nix config show`.

use crate::cmd;
use anyhow::Result;
use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct NixConfig {
    settings: BTreeMap<String, String>,
}

impl NixConfig {
    pub fn parse(text: &str) -> Self {
        let settings = text
            .lines()
            .filter_map(|l| l.split_once(" ="))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        NixConfig { settings }
    }

    pub fn load() -> Result<Self> {
        let text = cmd::output(
            Command::new("nix").args([
                "--extra-experimental-features",
                "nix-command",
                "config",
                "show",
            ]),
            Duration::from_secs(30),
        )?;
        Ok(Self::parse(&text))
    }

    /// A setting's value; empty values count as unset.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn flag(&self, key: &str) -> bool {
        self.get(key) == Some("true")
    }

    pub fn number(&self, key: &str) -> Option<u64> {
        self.get(key)?.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "auto-allocate-uids = true\nbuild-users-group = \nbuilders = @/etc/nix/machines\nid-count = 8388608\nmax-jobs = 32\nstart-id = 872415232\nkeep-derivations = true\nkeep-outputs = false\n";

    #[test]
    fn parses_settings() {
        let c = NixConfig::parse(SAMPLE);
        assert!(c.flag("auto-allocate-uids"));
        assert!(c.flag("keep-derivations"));
        assert!(!c.flag("keep-outputs"));
        assert!(!c.flag("missing"));
        assert_eq!(c.get("build-users-group"), None);
        assert_eq!(c.get("builders"), Some("@/etc/nix/machines"));
        assert_eq!(c.number("max-jobs"), Some(32));
        assert_eq!(c.number("start-id"), Some(872415232));
    }
}
```

- [ ] **Step 4: Write `src/users.rs`**

```rust
//! User and group names from /etc/passwd and /etc/group.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Users {
    names: HashMap<u32, String>,
    uids: HashMap<String, u32>,
    primary_gid: HashMap<u32, u32>,
    groups: HashMap<String, (u32, Vec<String>)>,
}

impl Users {
    pub fn read(etc_dir: &Path) -> Self {
        let passwd = fs::read_to_string(etc_dir.join("passwd")).unwrap_or_default();
        let group = fs::read_to_string(etc_dir.join("group")).unwrap_or_default();
        Self::parse(&passwd, &group)
    }

    pub fn parse(passwd: &str, group: &str) -> Self {
        let mut u = Users::default();
        for line in passwd.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 4 {
                continue;
            }
            if let (Ok(uid), Ok(gid)) = (f[2].parse::<u32>(), f[3].parse::<u32>()) {
                u.names.insert(uid, f[0].to_string());
                u.uids.insert(f[0].to_string(), uid);
                u.primary_gid.insert(uid, gid);
            }
        }
        for line in group.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() < 4 {
                continue;
            }
            if let Ok(gid) = f[2].parse::<u32>() {
                let members = f[3]
                    .split(',')
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .collect();
                u.groups.insert(f[0].to_string(), (gid, members));
            }
        }
        u
    }

    /// The user name for `uid`, or the number when it has none.
    pub fn name(&self, uid: u32) -> String {
        self.names
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }

    /// UIDs in `group`: listed members plus users whose primary group it is.
    pub fn group_member_uids(&self, group: &str) -> HashSet<u32> {
        let Some((gid, members)) = self.groups.get(group) else {
            return HashSet::new();
        };
        let mut out: HashSet<u32> = members
            .iter()
            .filter_map(|m| self.uids.get(m).copied())
            .collect();
        out.extend(
            self.primary_gid
                .iter()
                .filter(|(_, g)| *g == gid)
                .map(|(uid, _)| *uid),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0::/root:/bin/sh\nburke:x:1000:100::/home/burke:/bin/zsh\nnixbld1:x:30001:30000::/var/empty:/bin/false\nnixbld2:x:30002:65534::/var/empty:/bin/false\n";
    const GROUP: &str = "root:x:0:\nusers:x:100:\nnixbld:x:30000:nixbld2\n";

    #[test]
    fn names_and_group_members() {
        let u = Users::parse(PASSWD, GROUP);
        assert_eq!(u.name(1000), "burke");
        assert_eq!(u.name(872415232), "872415232");
        let members = u.group_member_uids("nixbld");
        assert!(members.contains(&30001), "primary group");
        assert!(members.contains(&30002), "listed member");
        assert!(!members.contains(&1000));
        assert!(u.group_member_uids("missing").is_empty());
    }
}
```

- [ ] **Step 5: Declare modules, run tests**

Add to `src/main.rs`: `mod cmd; mod nixconf; mod paths; mod users;`
Run: `nix develop -c cargo test`
Expected: all tests pass (metrics 4, cmd 4, nixconf 1, users 1).

- [ ] **Step 6: fmt, clippy, commit**

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add subprocess runner, paths, nix config and user lookups"
```

---

### Task 4: Process table

**Files:**
- Create: `src/procs.rs`, `src/testutil.rs`
- Modify: `src/main.rs` (add `mod procs;` and `#[cfg(test)] mod testutil;`)

- [ ] **Step 1: Write `src/testutil.rs`**

```rust
//! Helpers for building fake `/proc` and filesystem trees in tests.

use std::fs;
use std::path::Path;

pub fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// A `/proc/<pid>` with status, cmdline, stat (start time 5000 ticks) and an empty fd dir.
/// Every process has 1000 kB resident.
pub fn fake_proc(proc_dir: &Path, pid: u32, ppid: u32, uid: u32, name: &str, argv: &[&str]) {
    let d = proc_dir.join(pid.to_string());
    write(
        &d.join("status"),
        &format!("Name:\t{name}\nPPid:\t{ppid}\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nVmRSS:\t    1000 kB\n"),
    );
    let mut cmdline = argv.join("\0");
    cmdline.push('\0');
    write(&d.join("cmdline"), &cmdline);
    write(
        &d.join("stat"),
        &format!("{pid} ({name}) S {ppid} 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 5000 0"),
    );
    fs::create_dir_all(d.join("fd")).unwrap();
}

/// Makes `/proc/<pid>/fd/<fd>` a link to `socket:[inode]`.
pub fn fake_socket_fd(proc_dir: &Path, pid: u32, fd: u32, inode: u64) {
    std::os::unix::fs::symlink(
        format!("socket:[{inode}]"),
        proc_dir.join(pid.to_string()).join("fd").join(fd.to_string()),
    )
    .unwrap();
}
```

- [ ] **Step 2: Write `src/procs.rs`**

```rust
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
        (self.comm == "nix-daemon" && has("--stdio")) || (self.comm == "nix-store" && has("--serve"))
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
        .filter(|p| p.comm == "nix-daemon" && p.worker_client_pid().is_none() && !p.is_remote_server())
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
        fake_proc(p, 90, 1, 0, "determinate-nix", &["determinate-nixd", "daemon"]);
        fake_proc(p, 100, 90, 0, "nix-daemon", &["nix-daemon", "--option", "json-log-path", "/x"]);
        fake_proc(p, 200, 100, 0, "nix-daemon", &["nix-daemon", "4242\0", "json-log-path", "/x"]);
        fake_proc(p, 210, 100, 0, "crashpad_handle", &["crashpad_handler"]);
        fake_proc(p, 290, 1, 1001, "sshd-session", &["sshd-session: remotebuild@notty"]);
        fake_proc(p, 300, 290, 1001, "nix-daemon", &["nix-daemon", "--stdio"]);
        fake_proc(p, 400, 290, 1001, "nix-store", &["nix-store", "--serve", "--write"]);
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
```

- [ ] **Step 3: Declare modules and test**

Add `mod procs;` and `#[cfg(test)] mod testutil;` to `src/main.rs`.
Run: `nix develop -c cargo test procs`
Expected: 4 tests pass.

- [ ] **Step 4: fmt, clippy, commit**

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add /proc process table and daemon discovery"
```

---

### Task 5: TCP peers, reverse DNS, machines, locks

**Files:**
- Create: `src/nettcp.rs`, `src/resolve.rs`, `src/machines.rs`, `src/locks.rs`
- Modify: `src/main.rs` (add `mod locks; mod machines; mod nettcp; mod resolve;`)

- [ ] **Step 1: Write `src/nettcp.rs`**

```rust
//! Remote addresses of TCP sockets by inode, from `/proc/net/tcp{,6}`.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct TcpTable {
    by_inode: HashMap<u64, IpAddr>,
}

impl TcpTable {
    pub fn read(proc_dir: &Path) -> Self {
        let mut t = TcpTable::default();
        for f in ["net/tcp", "net/tcp6"] {
            if let Ok(text) = fs::read_to_string(proc_dir.join(f)) {
                t.add(&text);
            }
        }
        t
    }

    pub fn add(&mut self, text: &str) {
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // f[2] rem_address, f[3] state (0A = LISTEN), f[9] inode
            if f.len() < 10 || f[3] == "0A" {
                continue;
            }
            let ip = f[2].split_once(':').and_then(|(a, _)| parse_addr(a));
            if let (Some(ip), Ok(inode)) = (ip, f[9].parse::<u64>()) {
                if inode != 0 {
                    self.by_inode.insert(inode, ip);
                }
            }
        }
    }

    pub fn remote(&self, inode: u64) -> Option<IpAddr> {
        self.by_inode.get(&inode).copied()
    }
}

/// The kernel prints addresses as native-endian 32-bit words in hex.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(hex, 16).ok()?.to_ne_bytes(),
        ))),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                let w = u32::from_str_radix(&hex[i * 8..i * 8 + 8], 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
            }
            let v6 = Ipv6Addr::from(bytes);
            Some(v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // From juicy-j: sshd's connection from freddie-kane (100.74.146.55), and a listener.
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0EA57364:0016 37924A64:B354 01 00000000:00000000 02:000A3B2C 00000000     0        0 11035730 2 0000000000000000 20 4 30 10 -1\n   1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 37930 1 0000000000000000 100 0 0 10 0\n";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0000000000000000FFFF00000100007F:0016 0000000000000000FFFF00000100007F:9C40 01 00000000:00000000 00:00000000 00000000     0        0 555 1 0000000000000000 20 4 30 10 -1\n";

    #[test]
    fn parses_v4_and_v4_mapped_v6() {
        let mut t = TcpTable::default();
        t.add(TCP);
        t.add(TCP6);
        assert_eq!(t.remote(11035730), Some("100.74.146.55".parse().unwrap()));
        assert_eq!(t.remote(555), Some("127.0.0.1".parse().unwrap()));
        assert_eq!(t.remote(37930), None, "listeners are skipped");
    }
}
```

- [ ] **Step 2: Write `src/resolve.rs`**

```rust
//! Reverse DNS for peer addresses, cached for an hour; falls back to the address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(3600);

pub struct Resolver {
    enabled: bool,
    cache: Mutex<HashMap<IpAddr, (Instant, String)>>,
}

impl Resolver {
    pub fn new(enabled: bool) -> Self {
        Resolver {
            enabled,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn name(&self, ip: IpAddr) -> String {
        if !self.enabled {
            return ip.to_string();
        }
        let mut cache = self.cache.lock().unwrap();
        if let Some((at, name)) = cache.get(&ip) {
            if at.elapsed() < TTL {
                return name.clone();
            }
        }
        let name = dns_lookup::lookup_addr(&ip)
            .map(|n| n.trim_end_matches('.').to_string())
            .unwrap_or_else(|_| ip.to_string());
        cache.insert(ip, (Instant::now(), name.clone()));
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_returns_the_address() {
        let r = Resolver::new(false);
        assert_eq!(r.name("100.74.146.55".parse().unwrap()), "100.74.146.55");
    }
}
```

If `dns_lookup::lookup_addr` has a different signature in dns-lookup 4.x, check `nix develop -c cargo doc -p dns-lookup --open` or the crate source in `~/.cargo/registry` and adapt this one call; behaviour must stay "PTR name without trailing dot, else the address".

- [ ] **Step 3: Write `src/machines.rs`**

```rust
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
        write(&hydra, "localhost x86_64-linux - 16 1\nssh-ng://eu.nixbuild.net aarch64-linux - 5 1\n");
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
```

- [ ] **Step 4: Write `src/locks.rs`**

```rust
//! Inodes with a lock held, from `/proc/locks`.

use std::collections::HashSet;

/// Waiters (lines with `->`) are not holders.
pub fn held_inodes(text: &str) -> HashSet<u64> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.get(1) == Some(&"->") {
                return None;
            }
            // MAJOR:MINOR:INODE
            f.iter()
                .find(|t| t.matches(':').count() == 2)
                .and_then(|t| t.rsplit(':').next()?.parse().ok())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_holders_only() {
        let text = "1: FLOCK  ADVISORY  WRITE 819892 00:2a:20118213 0 EOF\n2: POSIX  ADVISORY  READ 1234 00:1b:555 0 EOF\n2: -> POSIX  ADVISORY  WRITE 99 00:1b:777 0 EOF\n";
        let held = held_inodes(text);
        assert!(held.contains(&20118213));
        assert!(held.contains(&555));
        assert!(!held.contains(&777));
        assert_eq!(held.len(), 2);
    }
}
```

- [ ] **Step 5: Declare modules and test**

Add `mod locks; mod machines; mod nettcp; mod resolve;` to `src/main.rs`.
Run: `nix develop -c cargo test`
Expected: all tests pass.

- [ ] **Step 6: fmt, clippy, commit**

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add TCP peer, reverse DNS, machines file and lock parsing"
```

---

### Task 6: Active builds, build processes, attribution

**Files:**
- Create: `src/active_builds.rs`, `src/buildset.rs`, `src/attribution.rs`
- Modify: `src/main.rs` (add `mod active_builds; mod attribution; mod buildset;`)

- [ ] **Step 1: Write `src/active_builds.rs`**

```rust
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
        assert!(b.cgroup.as_deref().unwrap().ends_with("nix-build-uid-872415232"));
    }

    #[test]
    fn missing_directory_is_none() {
        assert!(read_dir(Path::new("/nonexistent/active-builds")).is_none());
    }
}
```

- [ ] **Step 2: Write `src/buildset.rs`**

```rust
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
    out.extend(t.procs.values().filter(|p| uids.contains(p.uid)).map(|p| p.pid));
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
        .filter(|p| t.get(p.ppid).is_some_and(|parent| !uids.contains(parent.uid)))
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
        fake_proc(p, 300, 200, 872415232, "bash", &["bash", "-e", "builder.sh"]);
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
```

- [ ] **Step 3: Write `src/attribution.rs`**

```rust
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
            return self.for_client(pid);
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
            return Origin::new("remote", self.peer_of(pid).unwrap_or_default());
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
        fake_proc(p, 823418, 1, 0, "nix-daemon", &["nix-daemon", "--option", "json-log-path", "/x"]);
        // remote ssh-ng client
        fake_proc(p, 1686, 1, 0, "sshd", &["sshd: /nix/store/x/bin/sshd -D [listener]"]);
        fake_proc(p, 1320157, 1686, 0, "sshd-session", &["sshd-session: remotebuild [postauth]"]);
        fake_proc(p, 1320160, 1320157, 1001, "sshd-session", &["sshd-session: remotebuild@notty"]);
        fake_proc(p, 1320161, 1320160, 1001, "nix-daemon", &["nix-daemon", "--stdio"]);
        fake_proc(p, 1320186, 823418, 0, "nix-daemon", &["nix-daemon", "1320161", "json-log-path", "/x"]);
        fake_socket_fd(p, 1320160, 7, 11035730);
        // local client
        fake_proc(p, 1296115, 1, 1000, "nix", &["nix", "build"]);
        fake_proc(p, 1296284, 823418, 0, "nix-daemon", &["nix-daemon", "1296115", "json-log-path", "/x"]);
        // hydra
        fake_proc(p, 401264, 1, 2000, "hydra-queue-run", &["hydra-queue-runner", "-v"]);
        fake_proc(p, 401300, 823418, 0, "nix-daemon", &["nix-daemon", "401264", "json-log-path", "/x"]);
        // root building in-process
        fake_proc(p, 1296095, 1, 0, "nix", &["nix", "build"]);
        // worker whose client is gone
        fake_proc(p, 1400000, 823418, 0, "nix-daemon", &["nix-daemon", "999999", "json-log-path", "/x"]);
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
        assert_eq!(a.for_nix_pid(1320186), Origin::new("remote", "100.74.146.55"));
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
        assert_eq!(a.for_active_build(&b), Origin::new("remote", "100.74.146.55"));
        b.client_pid = None;
        b.client_uid = Some(1001);
        assert_eq!(a.for_active_build(&b), Origin::new("local", "remotebuild"));
    }
}
```

- [ ] **Step 4: Declare modules and test**

Add `mod active_builds; mod attribution; mod buildset;` to `src/main.rs`.
Run: `nix develop -c cargo test`
Expected: all tests pass.

- [ ] **Step 5: fmt, clippy, commit**

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add active builds, build process detection and build attribution"
```

---

### Task 7: cgroup and upkeep log parsing

**Files:**
- Create: `src/cgroup.rs`, `src/upkeep_log.rs`
- Modify: `src/main.rs` (add `mod cgroup; mod upkeep_log;`)

- [ ] **Step 1: Write `src/cgroup.rs`**

```rust
//! cgroup v2 metrics for the Nix daemon's unit.

use crate::metrics::Family;
use std::fs;
use std::path::{Path, PathBuf};

/// The cgroup v2 directory of `pid`, if it is in the unified hierarchy.
pub fn dir_of(proc_dir: &Path, cgroup_root: &Path, pid: u32) -> Option<PathBuf> {
    let text = fs::read_to_string(proc_dir.join(pid.to_string()).join("cgroup")).ok()?;
    let rel = text.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(cgroup_root.join(rel.trim_start_matches('/')))
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
            Some(PathBuf::from("/sys/fs/cgroup/system.slice/nix-daemon.service"))
        );
        assert_eq!(dir_of(&d.path().join("proc"), Path::new("/sys/fs/cgroup"), 101), None);
    }

    #[test]
    fn reads_counters_and_skips_missing_files() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path();
        write(&p.join("cpu.stat"), "usage_usec 1750000\nuser_usec 1500000\nsystem_usec 250000\n");
        write(&p.join("memory.current"), "1048576\n");
        write(&p.join("io.stat"), "259:0 rbytes=100 wbytes=200 rios=1 wios=2\n8:0 rbytes=1 wbytes=2 rios=0 wios=0\n");
        write(&p.join("cpu.pressure"), "some avg10=0.00 avg60=0.00 avg300=0.00 total=1500000\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
        let text = render(&families(p));
        assert!(text.contains("nix_daemon_cgroup_cpu_seconds_total{mode=\"user\"} 1.5\n"), "{text}");
        assert!(text.contains("nix_daemon_cgroup_cpu_seconds_total{mode=\"system\"} 0.25\n"));
        assert!(text.contains("nix_daemon_cgroup_memory_bytes 1048576\n"));
        assert!(!text.contains("nix_daemon_cgroup_memory_peak_bytes"));
        assert!(text.contains("nix_daemon_cgroup_io_bytes_total{direction=\"read\"} 101\n"));
        assert!(text.contains("nix_daemon_cgroup_io_bytes_total{direction=\"write\"} 202\n"));
        assert!(text.contains("nix_daemon_pressure_seconds_total{resource=\"cpu\",kind=\"some\"} 1.5\n"));
        assert!(text.contains("nix_daemon_pressure_seconds_total{resource=\"cpu\",kind=\"full\"} 0\n"));
        assert!(!text.contains("resource=\"memory\""));
    }
}
```

- [ ] **Step 2: Write `src/upkeep_log.rs`**

```rust
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
        if let Some(i) = t.windows(3).position(|w| w == ["store", "paths", "deleted,"]) {
            log.deleted_paths = i.checked_sub(1).and_then(|j| t[j].parse().ok());
            if let (Some(n), Some(u)) = (t.get(i + 3), t.get(i + 4)) {
                if let Some(b) = size(n, u) {
                    *log.freed_bytes.get_or_insert(0.0) += b;
                }
            }
        }
        // "2.2 GiB freed by hard-linking 618926 files"
        if let Some(i) = t.windows(3).position(|w| w == ["freed", "by", "hard-linking"]) {
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
        let log = parse_log("note: currently hard linking saves 1.50 MiB\n12 store paths deleted, 3.00 MiB freed\n");
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
```

- [ ] **Step 3: Declare modules, test, fmt, clippy, commit**

Add `mod cgroup; mod upkeep_log;` to `src/main.rs`.
Run: `nix develop -c cargo test`
Expected: all tests pass.

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add cgroup v2 and upkeep log parsing"
```

---

### Task 8: Store DB, liveness graph, profile links

**Files:**
- Create: `src/storedb.rs`, `src/liveness.rs`, `src/profiles.rs`
- Modify: `src/main.rs` (add `mod liveness; mod profiles; mod storedb;`)

- [ ] **Step 1: Write `src/storedb.rs`**

```rust
//! Read-only queries against Nix's SQLite store database.

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;
use std::time::Duration;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(1))?;
    Ok(conn)
}

#[derive(Debug, PartialEq)]
pub struct Stats {
    pub paths: f64,
    pub nar_bytes: f64,
    pub built: f64,
    pub signed: f64,
    pub last_registration: f64,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    Ok(conn.query_row(
        "select count(*), coalesce(sum(narSize), 0), coalesce(sum(ultimate = 1), 0), \
         coalesce(sum(coalesce(sigs, '') != ''), 0), coalesce(max(registrationTime), 0) \
         from ValidPaths",
        [],
        |r| {
            Ok(Stats {
                paths: r.get::<_, i64>(0)? as f64,
                nar_bytes: r.get::<_, i64>(1)? as f64,
                built: r.get::<_, i64>(2)? as f64,
                signed: r.get::<_, i64>(3)? as f64,
                last_registration: r.get::<_, i64>(4)? as f64,
            })
        },
    )?)
}

/// Total NAR size of `path`'s closure, or `None` when `path` is not valid.
pub fn closure_bytes(conn: &Connection, path: &str) -> Result<Option<f64>> {
    let id: Option<i64> = conn
        .query_row("select id from ValidPaths where path = ?1", [path], |r| r.get(0))
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    let sum: i64 = conn.query_row(
        "with recursive closure(id) as (select ?1 \
         union select reference from Refs join closure on Refs.referrer = closure.id) \
         select coalesce(sum(narSize), 0) from ValidPaths where id in (select id from closure)",
        [id],
        |r| r.get(0),
    )?;
    Ok(Some(sum as f64))
}

#[cfg(test)]
pub mod fixture {
    //! Nix's schema, and helpers to fill it.
    use rusqlite::{params, Connection};

    pub const SCHEMA: &str = "
        create table ValidPaths (id integer primary key autoincrement not null, path text unique not null,
            hash text not null, registrationTime integer not null, deriver text, narSize integer,
            ultimate integer, sigs text, ca text);
        create table Refs (referrer integer not null, reference integer not null,
            primary key (referrer, reference));
        create table DerivationOutputs (drv integer not null, id text not null, path text not null,
            primary key (drv, id));";

    /// A writable DB in WAL mode, like Nix's.
    pub fn create(path: &std::path::Path) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    /// Registration time is 100 + the number of paths already present.
    pub fn add_path(conn: &Connection, path: &str, nar: i64, ultimate: bool, signed: bool, deriver: Option<&str>) -> i64 {
        conn.execute(
            "insert into ValidPaths (path, hash, registrationTime, deriver, narSize, ultimate, sigs) \
             values (?1, 'sha256:0', (select count(*) from ValidPaths) + 100, ?2, ?3, ?4, ?5)",
            params![path, deriver, nar, ultimate.then_some(1), signed.then_some("cache:sig")],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    pub fn add_ref(conn: &Connection, from: i64, to: i64) {
        conn.execute("insert into Refs values (?1, ?2)", params![from, to]).unwrap();
    }

    pub fn add_output(conn: &Connection, drv: i64, name: &str, path: &str) {
        conn.execute("insert into DerivationOutputs values (?1, ?2, ?3)", params![drv, name, path]).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn stats_and_closure() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("db/db.sqlite");
        let w = create(&db);
        let a = add_path(&w, "/nix/store/a-app", 100, true, false, None);
        let b = add_path(&w, "/nix/store/b-lib", 50, false, true, None);
        let c = add_path(&w, "/nix/store/c-libc", 25, false, true, None);
        add_path(&w, "/nix/store/d-other", 7, false, false, None);
        add_ref(&w, a, b);
        add_ref(&w, a, a);
        add_ref(&w, b, c);
        // the writer stays open, as the daemon's does
        let r = open(&db).unwrap();
        assert_eq!(
            stats(&r).unwrap(),
            Stats {
                paths: 4.0,
                nar_bytes: 182.0,
                built: 1.0,
                signed: 2.0,
                last_registration: 103.0
            }
        );
        assert_eq!(closure_bytes(&r, "/nix/store/a-app").unwrap(), Some(175.0));
        assert_eq!(closure_bytes(&r, "/nix/store/c-libc").unwrap(), Some(25.0));
        assert_eq!(closure_bytes(&r, "/nix/store/missing").unwrap(), None);
    }
}
```

- [ ] **Step 2: Write `src/profiles.rs`**

```rust
//! Profile generation links.

use std::path::Path;

/// For `<dir>/<name>-<N>-link`, the profile path `<dir>/<name>` and generation `N`.
pub fn generation_link(link: &str) -> Option<(String, u64)> {
    let p = Path::new(link);
    let file = p.file_name()?.to_str()?;
    let (name, n) = file.strip_suffix("-link")?.rsplit_once('-')?;
    let n: u64 = n.parse().ok()?;
    if name.is_empty() {
        return None;
    }
    Some((p.with_file_name(name).to_string_lossy().into_owned(), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generation_links() {
        assert_eq!(
            generation_link("/nix/var/nix/profiles/system-55-link"),
            Some(("/nix/var/nix/profiles/system".to_string(), 55))
        );
        assert_eq!(
            generation_link("/home/b/.local/state/nix/profiles/home-manager-12-link"),
            Some(("/home/b/.local/state/nix/profiles/home-manager".to_string(), 12))
        );
        assert_eq!(generation_link("/nix/var/nix/profiles/system"), None);
        assert_eq!(generation_link("/x/foo-link"), None);
        assert_eq!(generation_link("/x/a-b-link"), None);
        assert_eq!(generation_link("/x/-3-link"), None);
    }
}
```

- [ ] **Step 3: Write `src/liveness.rs`**

```rust
//! What a garbage collection would delete.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// The store's reference graph, loaded into memory.
pub struct Graph {
    pub paths: Vec<String>,
    pub nar: Vec<u64>,
    refs: Vec<Vec<u32>>,
    deriver: Vec<Option<u32>>,
    outputs: Vec<Vec<u32>>,
    index: HashMap<String, u32>,
}

impl Graph {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut paths = Vec::new();
        let mut nar = Vec::new();
        let mut derivers: Vec<Option<String>> = Vec::new();
        let mut by_id: HashMap<i64, u32> = HashMap::new();
        let mut index: HashMap<String, u32> = HashMap::new();
        let mut stmt = conn.prepare("select id, path, coalesce(narSize, 0), deriver from ValidPaths")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let i = paths.len() as u32;
            let path: String = r.get(1)?;
            by_id.insert(r.get(0)?, i);
            index.insert(path.clone(), i);
            paths.push(path);
            nar.push(r.get::<_, i64>(2)? as u64);
            derivers.push(r.get(3)?);
        }
        let n = paths.len();
        let deriver = derivers
            .iter()
            .map(|d| d.as_ref().and_then(|d| index.get(d).copied()))
            .collect();
        let mut refs = vec![Vec::new(); n];
        let mut stmt = conn.prepare("select referrer, reference from Refs")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            if let (Some(&a), Some(&b)) = (by_id.get(&r.get(0)?), by_id.get(&r.get(1)?)) {
                refs[a as usize].push(b);
            }
        }
        let mut outputs = vec![Vec::new(); n];
        let mut stmt = conn.prepare("select drv, path from DerivationOutputs")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let path: String = r.get(1)?;
            if let (Some(&d), Some(&o)) = (by_id.get(&r.get(0)?), index.get(&path)) {
                outputs[d as usize].push(o);
            }
        }
        Ok(Graph {
            paths,
            nar,
            refs,
            deriver,
            outputs,
            index,
        })
    }

    /// Indices of paths not reachable from `roots`. With `keep_derivations`
    /// the derivers of live paths are live; with `keep_outputs` the outputs
    /// of live derivations are live (Nix's GC settings).
    pub fn dead(&self, roots: &[String], keep_derivations: bool, keep_outputs: bool) -> Vec<u32> {
        let mut alive = vec![false; self.paths.len()];
        let mut stack: Vec<u32> = roots.iter().filter_map(|r| self.index.get(r).copied()).collect();
        while let Some(i) = stack.pop() {
            let iu = i as usize;
            if alive[iu] {
                continue;
            }
            alive[iu] = true;
            stack.extend(&self.refs[iu]);
            if keep_derivations {
                stack.extend(self.deriver[iu]);
            }
            if keep_outputs {
                stack.extend(&self.outputs[iu]);
            }
        }
        (0..self.paths.len() as u32)
            .filter(|i| !alive[*i as usize])
            .collect()
    }
}

/// `nix-store --gc --print-roots` lines: `<link> -> <store path>`.
pub fn parse_roots(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.rsplit_once(" -> "))
        .map(|(link, target)| (link.to_string(), target.to_string()))
        .collect()
}

/// The store path containing `p` (`<store_dir>/<name>`), if `p` is in the store.
pub fn store_path_of(p: &str, store_dir: &str) -> Option<String> {
    let rest = p.strip_prefix(store_dir)?.strip_prefix('/')?;
    let name = rest.split('/').next().filter(|n| !n.is_empty())?;
    Some(format!("{store_dir}/{name}"))
}

/// Store paths in `temproots/`: one file per process, NUL-terminated paths.
pub fn temp_roots(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| fs::read(e.path()).ok())
        .flat_map(|b| {
            b.split(|c| *c == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The `kind` label for a root's link.
pub fn root_kind(link: &str, state_dir: &str) -> &'static str {
    if link.starts_with("/proc/") || link.starts_with("{censored}") {
        "runtime"
    } else if crate::profiles::generation_link(link).is_some() {
        "profile"
    } else if link.contains("/gcroots/hydra/") {
        "hydra"
    } else if link.starts_with(state_dir) || link.starts_with("/run/") {
        "other"
    } else {
        "auto"
    }
}

/// `gcroots/auto` links whose target no longer exists. Must run before
/// `--print-roots`, which deletes them.
pub fn stale_auto_roots(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| fs::metadata(e.path()).is_err())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storedb::{self, fixture::*};
    use crate::testutil::write;

    /// app -> lib -> libc; drv built app; old is unreferenced; tool.drv's
    /// output is tool.
    fn graph() -> (tempfile::TempDir, Graph) {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("db.sqlite");
        let w = create(&db);
        let drv = add_path(&w, "/nix/store/d-app.drv", 1, false, false, None);
        let app = add_path(&w, "/nix/store/a-app", 100, false, false, Some("/nix/store/d-app.drv"));
        let lib = add_path(&w, "/nix/store/b-lib", 50, false, false, None);
        let libc = add_path(&w, "/nix/store/c-libc", 25, false, false, None);
        add_path(&w, "/nix/store/e-old", 7, false, false, None);
        let tool_drv = add_path(&w, "/nix/store/f-tool.drv", 1, false, false, None);
        add_path(&w, "/nix/store/g-tool", 3, false, false, Some("/nix/store/f-tool.drv"));
        add_ref(&w, app, lib);
        add_ref(&w, lib, libc);
        add_ref(&w, lib, lib);
        add_output(&w, tool_drv, "out", "/nix/store/g-tool");
        add_output(&w, drv, "out", "/nix/store/a-app");
        let g = Graph::load(&storedb::open(&db).unwrap()).unwrap();
        (d, g)
    }

    fn names(g: &Graph, dead: &[u32]) -> Vec<String> {
        let mut v: Vec<String> = dead.iter().map(|i| g.paths[*i as usize].clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn walks_references() {
        let (_d, g) = graph();
        let dead = g.dead(&["/nix/store/a-app".into(), "/nix/store/missing".into()], false, false);
        assert_eq!(
            names(&g, &dead),
            vec!["/nix/store/d-app.drv", "/nix/store/e-old", "/nix/store/f-tool.drv", "/nix/store/g-tool"]
        );
    }

    #[test]
    fn keep_derivations_and_outputs() {
        let (_d, g) = graph();
        let dead = g.dead(&["/nix/store/a-app".into()], true, false);
        assert_eq!(
            names(&g, &dead),
            vec!["/nix/store/e-old", "/nix/store/f-tool.drv", "/nix/store/g-tool"]
        );
        let dead = g.dead(&["/nix/store/f-tool.drv".into()], false, true);
        assert!(!names(&g, &dead).contains(&"/nix/store/g-tool".to_string()));
    }

    #[test]
    fn parses_roots_and_store_paths() {
        let roots = parse_roots("/proc/2453/maps -> /nix/store/x-glibc-2.42\n/nix/var/nix/profiles/system-55-link -> /nix/store/y-nixos-system\n{censored} -> /nix/store/z-thing\n");
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0], ("/proc/2453/maps".into(), "/nix/store/x-glibc-2.42".into()));
        assert_eq!(
            store_path_of("/nix/store/x-glibc-2.42/lib/libc.so.6", "/nix/store").as_deref(),
            Some("/nix/store/x-glibc-2.42")
        );
        assert_eq!(store_path_of("/home/x", "/nix/store"), None);
        assert_eq!(store_path_of("/nix/store/", "/nix/store"), None);
    }

    #[test]
    fn classifies_roots() {
        let s = "/nix/var/nix";
        assert_eq!(root_kind("/proc/2453/maps", s), "runtime");
        assert_eq!(root_kind("{censored}", s), "runtime");
        assert_eq!(root_kind("/nix/var/nix/profiles/system-55-link", s), "profile");
        assert_eq!(root_kind("/home/b/.local/state/nix/profiles/home-manager-3-link", s), "profile");
        assert_eq!(root_kind("/nix/var/nix/gcroots/hydra/abc-nixos-system.drv", s), "hydra");
        assert_eq!(root_kind("/run/booted-system", s), "other");
        assert_eq!(root_kind("/home/b/src/x/result", s), "auto");
    }

    #[test]
    fn temp_and_stale_roots() {
        let d = tempfile::tempdir().unwrap();
        write(&d.path().join("temproots/123"), "/nix/store/a-x\0/nix/store/b-y\0");
        let mut t = temp_roots(&d.path().join("temproots"));
        t.sort();
        assert_eq!(t, vec!["/nix/store/a-x", "/nix/store/b-y"]);
        let auto = d.path().join("auto");
        std::fs::create_dir_all(&auto).unwrap();
        std::os::unix::fs::symlink(d.path().join("temproots/123"), auto.join("ok")).unwrap();
        std::os::unix::fs::symlink(d.path().join("gone"), auto.join("stale")).unwrap();
        assert_eq!(stale_auto_roots(&auto), 1);
        assert_eq!(stale_auto_roots(&d.path().join("nope")), 0);
    }
}
```

- [ ] **Step 4: Declare modules, test, fmt, clippy, commit**

Add `mod liveness; mod profiles; mod storedb;` to `src/main.rs`.
Run: `nix develop -c cargo test`
Expected: all tests pass.

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add store DB queries, liveness graph and profile link parsing"
```

---

### Task 9: Collector framework and snapshot file

**Files:**
- Create: `src/collector.rs`
- Modify: `src/main.rs` (add `mod collector;`)

- [ ] **Step 1: Write `src/collector.rs`**

```rust
//! The collector trait, shared context, and the snapshot file writer.

use crate::metrics::{render, Family};
use crate::nixconf::NixConfig;
use crate::paths::Paths;
use crate::resolve::Resolver;
use anyhow::Result;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONFIG_TTL: Duration = Duration::from_secs(300);

pub trait Collector: Send {
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;
    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>>;
    /// Whether to keep serving the last good result when a cycle fails.
    fn keep_on_failure(&self) -> bool {
        false
    }
}

/// What all collectors share.
pub struct Ctx {
    pub paths: Paths,
    pub extra_machines: Vec<PathBuf>,
    pub upkeep_units: Vec<String>,
    pub resolver: Resolver,
    config: Mutex<Option<(Instant, Arc<NixConfig>)>>,
}

impl Ctx {
    pub fn new(paths: Paths, extra_machines: Vec<PathBuf>, upkeep_units: Vec<String>, resolver: Resolver) -> Self {
        Ctx {
            paths,
            extra_machines,
            upkeep_units,
            resolver,
            config: Mutex::new(None),
        }
    }

    /// `nix config show`, cached for five minutes.
    pub fn nix_config(&self) -> Result<Arc<NixConfig>> {
        let mut c = self.config.lock().unwrap();
        if let Some((at, cfg)) = c.as_ref() {
            if at.elapsed() < CONFIG_TTL {
                return Ok(cfg.clone());
            }
        }
        let cfg = Arc::new(NixConfig::load()?);
        *c = Some((Instant::now(), cfg.clone()));
        Ok(cfg)
    }

    /// A context over fixture paths with a fixed Nix config.
    #[cfg(test)]
    pub fn for_test(paths: Paths, config: &str) -> Self {
        let ctx = Ctx::new(paths, Vec::new(), Vec::new(), Resolver::new(false));
        *ctx.config.lock().unwrap() = Some((Instant::now(), Arc::new(NixConfig::parse(config))));
        ctx
    }
}

/// A Nix command that goes through the daemon (see the spec: under
/// `ProtectSystem=strict` a root client cannot open the local store).
pub fn nix_command(program: &str) -> Command {
    let mut c = Command::new(program);
    c.env("NIX_REMOTE", "daemon");
    c
}

pub fn build_info() -> Family {
    Family::gauge("nix_exporter_build_info", "Version of nix-exporter.")
        .with(&[("version", env!("CARGO_PKG_VERSION"))], 1.0)
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct Entry {
    families: Vec<Family>,
    success: bool,
    duration: f64,
    last_success: Option<f64>,
}

/// The merged output of all collectors, rewritten atomically after every update.
pub struct Snapshot {
    path: PathBuf,
    entries: Mutex<BTreeMap<&'static str, Entry>>,
}

impl Snapshot {
    pub fn new(path: PathBuf) -> Self {
        Snapshot {
            path,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn update(
        &self,
        name: &'static str,
        result: Result<Vec<Family>>,
        duration: f64,
        keep_on_failure: bool,
    ) -> std::io::Result<()> {
        let mut entries = self.entries.lock().unwrap();
        let prev = entries.remove(name);
        let entry = match result {
            Ok(families) => Entry {
                families,
                success: true,
                duration,
                last_success: Some(now()),
            },
            Err(e) => {
                eprintln!("collector {name} failed: {e:#}");
                let families = match (&prev, keep_on_failure) {
                    (Some(p), true) => p.families.clone(),
                    _ => Vec::new(),
                };
                Entry {
                    families,
                    success: false,
                    duration,
                    last_success: prev.and_then(|p| p.last_success),
                }
            }
        };
        entries.insert(name, entry);
        // Written under the lock so concurrent updates cannot interleave.
        self.write(&Self::render(&entries))
    }

    fn render(entries: &BTreeMap<&'static str, Entry>) -> String {
        let mut families: Vec<Family> = entries
            .values()
            .flat_map(|e| e.families.iter().cloned())
            .collect();
        let mut success = Family::gauge(
            "nix_exporter_collector_success",
            "Whether the collector's last cycle succeeded.",
        );
        let mut duration = Family::gauge(
            "nix_exporter_collector_duration_seconds",
            "How long the collector's last cycle took.",
        );
        let mut last = Family::gauge(
            "nix_exporter_collector_last_success_timestamp_seconds",
            "When the collector last succeeded.",
        );
        for (name, e) in entries {
            success.push(&[("collector", name)], if e.success { 1.0 } else { 0.0 });
            duration.push(&[("collector", name)], e.duration);
            if let Some(t) = e.last_success {
                last.push(&[("collector", name)], t);
            }
        }
        families.extend([success, duration, last, build_info()]);
        render(&families)
    }

    fn write(&self, text: &str) -> std::io::Result<()> {
        let tmp = self.path.with_extension("prom.tmp");
        fs::write(&tmp, text)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
        fs::rename(&tmp, &self.path)
    }
}

/// Runs each collector on its own thread, forever.
pub fn run(collectors: Vec<Box<dyn Collector>>, ctx: Arc<Ctx>, snapshot: Arc<Snapshot>) -> ! {
    for mut c in collectors {
        let ctx = ctx.clone();
        let snapshot = snapshot.clone();
        thread::spawn(move || loop {
            let start = Instant::now();
            let result = c.collect(&ctx);
            let elapsed = start.elapsed();
            if let Err(e) = snapshot.update(c.name(), result, elapsed.as_secs_f64(), c.keep_on_failure()) {
                eprintln!("writing {}: {e}", snapshot.path.display());
            }
            thread::sleep(c.interval().saturating_sub(elapsed));
        });
    }
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    fn fam() -> Vec<Family> {
        vec![Family::gauge("nix_test", "Test.").with(&[], 7.0)]
    }

    #[test]
    fn writes_merged_snapshot_with_status() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("a", Ok(fam()), 0.5, false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("nix_test 7\n"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"a\"} 1\n"));
        assert!(text.contains("nix_exporter_collector_duration_seconds{collector=\"a\"} 0.5\n"));
        assert!(text.contains("nix_exporter_collector_last_success_timestamp_seconds{collector=\"a\"}"));
        assert!(text.contains("nix_exporter_build_info{version=\"0.1.0\"} 1\n"));
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert!(!d.path().join("metrics.prom.tmp").exists());
    }

    #[test]
    fn failure_drops_metrics_but_keeps_last_success() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("a", Ok(fam()), 0.1, false).unwrap();
        s.update("a", Err(anyhow!("boom")), 0.1, false).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("nix_test 7"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"a\"} 0\n"));
        assert!(text.contains("nix_exporter_collector_last_success_timestamp_seconds{collector=\"a\"}"));
    }

    #[test]
    fn keep_on_failure_keeps_metrics() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("metrics.prom");
        let s = Snapshot::new(path.clone());
        s.update("slow", Ok(fam()), 0.1, true).unwrap();
        s.update("slow", Err(anyhow!("boom")), 0.1, true).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("nix_test 7\n"));
        assert!(text.contains("nix_exporter_collector_success{collector=\"slow\"} 0\n"));
    }
}
```

- [ ] **Step 2: Declare module, test, fmt, clippy, commit**

Add `mod collector;` to `src/main.rs`.
Run: `nix develop -c cargo test collector`
Expected: 3 tests pass.

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add collector framework and atomic snapshot writer"
```

---

### Task 10: Store, liveness and profiles collectors

**Files:**
- Create: `src/collectors/mod.rs`, `src/collectors/store.rs`, `src/collectors/liveness.rs`, `src/collectors/profiles.rs`
- Modify: `src/main.rs` (add `mod collectors;`)

- [ ] **Step 1: Write `src/collectors/mod.rs`** (later tasks add the other collectors to it)

```rust
//! One module per collector.

mod liveness;
mod profiles;
mod store;

pub use liveness::dead_paths;

use crate::collector::Collector;
use std::time::Duration;

/// Every collector. `base` is the fast interval; slower collectors use 4x.
pub fn all(base: Duration, liveness: Duration) -> Vec<Box<dyn Collector>> {
    let slow = base * 4;
    vec![
        Box::new(store::Store { interval: base }),
        Box::new(profiles::Profiles { interval: slow }),
        Box::new(liveness::Liveness { interval: liveness }),
    ]
}
```

- [ ] **Step 2: Write `src/collectors/store.rs`**

```rust
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::storedb;
use anyhow::Result;
use std::fs;
use std::path::Path;
use std::time::Duration;

pub struct Store {
    pub interval: Duration,
}

fn size(p: &Path) -> f64 {
    fs::metadata(p).map(|m| m.len() as f64).unwrap_or(0.0)
}

impl Collector for Store {
    fn name(&self) -> &'static str {
        "store"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let db = ctx.paths.db();
        let s = storedb::stats(&storedb::open(&db)?)?;
        let db_bytes = size(&db) + size(&db.with_extension("sqlite-wal"));
        Ok(vec![
            Family::gauge("nix_store_valid_paths", "Valid store paths.").with(&[], s.paths),
            Family::gauge("nix_store_nar_bytes", "Total NAR size of valid store paths.")
                .with(&[], s.nar_bytes),
            Family::gauge("nix_store_paths", "Valid store paths by whether this host built them.")
                .with(&[("origin", "built")], s.built)
                .with(&[("origin", "substituted")], s.paths - s.built),
            Family::gauge("nix_store_signed_paths", "Valid store paths with at least one signature.")
                .with(&[], s.signed),
            Family::gauge(
                "nix_store_last_registration_timestamp_seconds",
                "When the newest store path was registered.",
            )
            .with(&[], s.last_registration),
            Family::gauge("nix_store_db_bytes", "Size of the store database and its write-ahead log.")
                .with(&[], db_bytes),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::paths::Paths;
    use crate::storedb::fixture::*;

    #[test]
    fn reports_store_stats() {
        let d = tempfile::tempdir().unwrap();
        let paths = Paths {
            state_dir: d.path().to_path_buf(),
            ..Paths::default()
        };
        let w = create(&paths.db());
        add_path(&w, "/nix/store/a-x", 10, true, false, None);
        add_path(&w, "/nix/store/b-y", 20, false, true, None);
        let ctx = Ctx::for_test(paths, "");
        let text = render(&Store { interval: Duration::from_secs(1) }.collect(&ctx).unwrap());
        assert!(text.contains("nix_store_valid_paths 2\n"), "{text}");
        assert!(text.contains("nix_store_nar_bytes 30\n"));
        assert!(text.contains("nix_store_paths{origin=\"built\"} 1\n"));
        assert!(text.contains("nix_store_paths{origin=\"substituted\"} 1\n"));
        assert!(text.contains("nix_store_signed_paths 1\n"));
        assert!(text.contains("nix_store_last_registration_timestamp_seconds 101\n"));
        assert!(text.contains("nix_store_db_bytes "));
    }
}
```

- [ ] **Step 3: Write `src/collectors/liveness.rs`**

```rust
use crate::cmd;
use crate::collector::{nix_command, Collector, Ctx};
use crate::liveness::{self, Graph};
use crate::metrics::Family;
use crate::storedb;
use anyhow::Result;
use std::collections::BTreeMap;
use std::time::Duration;

pub struct Liveness {
    pub interval: Duration,
}

/// `(link, store path)` for every GC root, via the daemon.
pub fn print_roots() -> Result<Vec<(String, String)>> {
    let text = cmd::output(
        nix_command("nix-store").args(["--gc", "--print-roots"]),
        Duration::from_secs(120),
    )?;
    Ok(liveness::parse_roots(&text))
}

struct Computed {
    graph: Graph,
    dead: Vec<u32>,
    roots: Vec<(String, String)>,
    stale: usize,
}

fn compute(ctx: &Ctx) -> Result<Computed> {
    // Order matters: stale links before --print-roots deletes them, temp
    // roots before loading the graph so paths built meanwhile stay alive.
    let stale = liveness::stale_auto_roots(&ctx.paths.gcroots_auto());
    let roots = print_roots()?;
    let store = ctx.paths.store_dir.as_str();
    let mut root_paths: Vec<String> = roots
        .iter()
        .filter_map(|(_, t)| liveness::store_path_of(t, store))
        .collect();
    root_paths.extend(
        liveness::temp_roots(&ctx.paths.temproots())
            .iter()
            .filter_map(|t| liveness::store_path_of(t, store)),
    );
    let cfg = ctx.nix_config()?;
    let graph = Graph::load(&storedb::open(&ctx.paths.db())?)?;
    let dead = graph.dead(&root_paths, cfg.flag("keep-derivations"), cfg.flag("keep-outputs"));
    Ok(Computed {
        graph,
        dead,
        roots,
        stale,
    })
}

/// The dead store paths, sorted; for `nix-exporter print-dead`.
pub fn dead_paths(ctx: &Ctx) -> Result<Vec<String>> {
    let c = compute(ctx)?;
    let mut v: Vec<String> = c.dead.iter().map(|i| c.graph.paths[*i as usize].clone()).collect();
    v.sort();
    Ok(v)
}

impl Collector for Liveness {
    fn name(&self) -> &'static str {
        "liveness"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn keep_on_failure(&self) -> bool {
        true
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let c = compute(ctx)?;
        let dead_bytes: u64 = c.dead.iter().map(|i| c.graph.nar[*i as usize]).sum();
        let state_dir = ctx.paths.state_dir.to_string_lossy();
        let mut kinds: BTreeMap<&str, f64> = BTreeMap::new();
        for (link, _) in &c.roots {
            *kinds.entry(liveness::root_kind(link, &state_dir)).or_default() += 1.0;
        }
        let mut roots = Family::gauge("nix_store_gc_roots", "GC roots by kind.");
        for (kind, n) in &kinds {
            roots.push(&[("kind", kind)], *n);
        }
        Ok(vec![
            Family::gauge("nix_store_dead_paths", "Valid store paths not reachable from any GC root.")
                .with(&[], c.dead.len() as f64),
            Family::gauge("nix_store_dead_bytes", "NAR size of store paths a GC would delete.")
                .with(&[], dead_bytes as f64),
            roots,
            Family::gauge("nix_store_gc_roots_stale", "gcroots/auto links whose target is gone.")
                .with(&[], c.stale as f64),
        ])
    }
}
```

This collector shells out to `nix-store`, so it has no unit test; its parts are tested in `src/liveness.rs`, and the NixOS test (Task 15) compares `print-dead` with `nix-store --gc --print-dead`.

- [ ] **Step 4: Write `src/collectors/profiles.rs`**

```rust
use super::liveness::print_roots;
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::profiles::generation_link;
use crate::storedb;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

pub struct Profiles {
    pub interval: Duration,
}

fn mtime(link: &str) -> Option<f64> {
    let m = fs::symlink_metadata(link).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// The generation the profile symlink points at, among `links`.
fn current_generation(profile: &str, links: &[(u64, String)]) -> Option<(u64, String)> {
    let target = fs::read_link(profile).ok()?;
    // A relative target resolves against the profile's directory; an
    // absolute one replaces the path entirely.
    let target = Path::new(profile).with_file_name(target);
    let (_, n) = generation_link(&target.to_string_lossy())?;
    links.iter().find(|(m, _)| *m == n).cloned()
}

/// Metrics for the profiles named by GC root links.
pub fn summarize(roots: &[(String, String)], conn: &Connection) -> Result<Vec<Family>> {
    let mut by_profile: BTreeMap<String, Vec<(u64, String)>> = BTreeMap::new();
    for (link, _) in roots {
        if let Some((profile, n)) = generation_link(link) {
            by_profile.entry(profile).or_default().push((n, link.clone()));
        }
    }
    let mut gens = Family::gauge("nix_profile_generations", "Generations of the profile.");
    let mut current = Family::gauge("nix_profile_current_generation", "The profile's current generation.");
    let mut current_ts = Family::gauge(
        "nix_profile_current_timestamp_seconds",
        "When the current generation was created.",
    );
    let mut oldest_ts = Family::gauge(
        "nix_profile_oldest_timestamp_seconds",
        "When the oldest remaining generation was created.",
    );
    let mut closure = Family::gauge(
        "nix_profile_closure_bytes",
        "NAR size of the current generation's closure.",
    );
    for (profile, links) in &by_profile {
        let l = [("profile", profile.as_str())];
        gens.push(&l, links.len() as f64);
        if let Some(t) = links.iter().filter_map(|(_, link)| mtime(link)).reduce(f64::min) {
            oldest_ts.push(&l, t);
        }
        let Some((n, link)) = current_generation(profile, links) else {
            continue;
        };
        current.push(&l, n as f64);
        if let Some(t) = mtime(&link) {
            current_ts.push(&l, t);
        }
        if let Ok(target) = fs::read_link(&link) {
            if let Some(bytes) = storedb::closure_bytes(conn, &target.to_string_lossy())? {
                closure.push(&l, bytes);
            }
        }
    }
    Ok(vec![gens, current, current_ts, oldest_ts, closure])
}

impl Collector for Profiles {
    fn name(&self) -> &'static str {
        "profiles"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        summarize(&print_roots()?, &storedb::open(&ctx.paths.db())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;
    use crate::storedb::fixture::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn summarizes_generations() {
        let d = tempfile::tempdir().unwrap();
        let w = create(&d.path().join("db.sqlite"));
        let sys = add_path(&w, "/nix/store/s2-system", 100, false, false, None);
        let dep = add_path(&w, "/nix/store/dep", 23, false, false, None);
        add_ref(&w, sys, dep);
        let profiles = d.path().join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        symlink("/nix/store/s1-system", profiles.join("system-1-link")).unwrap();
        symlink("/nix/store/s2-system", profiles.join("system-2-link")).unwrap();
        symlink("system-2-link", profiles.join("system")).unwrap();
        let p = |s: &str| profiles.join(s).to_string_lossy().into_owned();
        let roots = vec![
            (p("system-1-link"), "/nix/store/s1-system".to_string()),
            (p("system-2-link"), "/nix/store/s2-system".to_string()),
            (p("system"), "/nix/store/s2-system".to_string()),
            ("/proc/1/maps".to_string(), "/nix/store/dep".to_string()),
        ];
        let conn = storedb::open(&d.path().join("db.sqlite")).unwrap();
        let text = render(&summarize(&roots, &conn).unwrap());
        let label = format!("{{profile=\"{}\"}}", p("system"));
        assert!(text.contains(&format!("nix_profile_generations{label} 2\n")), "{text}");
        assert!(text.contains(&format!("nix_profile_current_generation{label} 2\n")));
        assert!(text.contains(&format!("nix_profile_closure_bytes{label} 123\n")));
        assert!(text.contains(&format!("nix_profile_current_timestamp_seconds{label} ")));
        assert!(text.contains(&format!("nix_profile_oldest_timestamp_seconds{label} ")));
    }
}
```

- [ ] **Step 5: Declare module, test, fmt, clippy, commit**

Add `mod collectors;` to `src/main.rs`.
Run: `nix develop -c cargo test`
Expected: all tests pass.

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add store, liveness and profiles collectors"
```

---

### Task 11: Daemon and builds collectors

**Files:**
- Create: `src/collectors/daemon.rs`, `src/collectors/builds.rs`
- Modify: `src/collectors/mod.rs`

- [ ] **Step 1: Write `src/collectors/daemon.rs`**

```rust
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
    /// (main daemon pid, version, variant), refreshed when the daemon restarts.
    version: Option<(u32, String, &'static str)>,
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
        let cfg = ctx.nix_config()?;
        let procs = ProcTable::read(&ctx.paths.proc_dir);
        let users = Users::read(&ctx.paths.etc_dir);
        let d = find_daemon(&procs);
        let mut out = vec![Family::gauge("nix_daemon_up", "Whether the Nix daemon process was found.")
            .with(&[], if d.main.is_some() { 1.0 } else { 0.0 })];
        if let Some(n) = cfg.number("max-jobs") {
            out.push(Family::gauge("nix_daemon_max_jobs", "The max-jobs setting.").with(&[], n as f64));
        }
        let Some(main) = d.main else {
            return Ok(out);
        };

        if self.version.as_ref().is_none_or(|(pid, ..)| *pid != main) {
            let exe = ctx.paths.proc_dir.join(main.to_string()).join("exe");
            let text = cmd::output(Command::new(exe).arg("--version"), Duration::from_secs(10))
                .unwrap_or_default();
            self.version = parse_version(&text).map(|(v, k)| (main, v, k));
        }
        if let Some((_, version, variant)) = &self.version {
            out.push(
                Family::gauge("nix_daemon_info", "Nix daemon version.")
                    .with(&[("version", version), ("variant", variant)], 1.0),
            );
        }
        if let (Some(boot), Some(p)) = (procs::boot_time(&ctx.paths.proc_dir), procs.get(main)) {
            out.push(
                Family::gauge("nix_daemon_start_time_seconds", "When the Nix daemon started.")
                    .with(&[], procs::start_time_seconds(p, boot)),
            );
        }
        out.push(
            Family::gauge("nix_daemon_connections", "Open daemon connections (worker processes).")
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
            let peer = attr.peer_of(client.pid).unwrap_or_default();
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

        let uids = BuildUids::from_config(&cfg, &users);
        let mains: Vec<u32> = active_builds::read_dir(&ctx.paths.active_builds())
            .unwrap_or_default()
            .iter()
            .filter_map(|b| b.main_pid)
            .collect();
        let builds = build_pids(&procs, &mains, &uids);
        let rss = |pids: &mut dyn Iterator<Item = u32>| -> f64 {
            pids.filter_map(|p| procs.get(p)).map(|p| p.rss_bytes as f64).sum()
        };
        let daemon_rss = rss(&mut std::iter::once(main).chain(d.workers.iter().copied()));
        let builds_rss = rss(&mut builds.iter().copied());
        out.push(
            Family::gauge("nix_daemon_memory_bytes", "Resident memory of the daemon and of running builds.")
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
        fake_proc(p, 290, 1, 1001, "sshd-session", &["sshd-session: remotebuild@notty"]);
        fake_proc(p, 300, 290, 1001, "nix-daemon", &["nix-daemon", "--stdio"]);
        fake_proc(p, 200, 100, 0, "nix-daemon", &["nix-daemon", "300"]);
        fake_socket_fd(p, 290, 7, 11035730);
        write(&p.join("net/tcp"), "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0EA57364:0016 37924A64:B354 01 00000000:00000000 02:000A3B2C 00000000     0        0 11035730 2 0000000000000000 20 4 30 10 -1\n");
        // a build running as an auto-allocated UID under the worker
        fake_proc(p, 400, 200, 872415232, "bash", &["bash"]);
        write(&p.join("stat"), "btime 1792400000\n");
        write(&p.join("100/cgroup"), "0::/system.slice/nix-daemon.service\n");
        write(&root.join("cgroup/system.slice/nix-daemon.service/cpu.stat"), "user_usec 2000000\nsystem_usec 1000000\n");
        write(&root.join("etc/passwd"), "root:x:0:0::/:/x\nremotebuild:x:1001:100::/:/x\n");
        let paths = Paths {
            proc_dir: proc_dir.clone(),
            cgroup_root: root.join("cgroup"),
            state_dir: root.join("state"),
            etc_dir: root.join("etc"),
            ..Paths::default()
        };
        let ctx = Ctx::for_test(paths, "max-jobs = 32\nauto-allocate-uids = true\nstart-id = 872415232\nid-count = 8388608\n");
        let text = render(&Daemon::new(Duration::from_secs(1)).collect(&ctx).unwrap());
        assert!(text.contains("nix_daemon_up 1\n"), "{text}");
        assert!(text.contains("nix_daemon_max_jobs 32\n"));
        assert!(text.contains("nix_daemon_connections 1\n"));
        assert!(text.contains("nix_daemon_remote_sessions{user=\"remotebuild\",client=\"100.74.146.55\"} 1\n"));
        assert!(text.contains("nix_daemon_memory_bytes{role=\"daemon\"} 2048000\n"));
        assert!(text.contains("nix_daemon_memory_bytes{role=\"builds\"} 1024000\n"));
        assert!(text.contains("nix_daemon_cgroup_cpu_seconds_total{mode=\"user\"} 2\n"));
        assert!(text.contains("nix_daemon_start_time_seconds "));
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
```

- [ ] **Step 2: Write `src/collectors/builds.rs`**

```rust
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
                found.extend(builds.iter().map(|b| (attr.for_active_build(b), b.start_time)));
            }
            None => {
                // Upstream Nix: no active-builds/, find builds from processes.
                let uids = BuildUids::from_config(&ctx.nix_config()?, &users);
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
        write(&root.join("etc/passwd"), "root:x:0:0::/:/x\nburke:x:1000:100::/:/x\n");
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
        let text = render(&Builds { interval: Duration::from_secs(1) }.collect(&ctx).unwrap());
        assert!(text.contains("nix_builds_active{origin=\"local\",client=\"burke\"} 1\n"), "{text}");
        assert!(text.contains("nix_build_oldest_start_timestamp_seconds 1792482889\n"));
    }

    #[test]
    fn falls_back_to_processes() {
        let d = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(setup(d.path()), AUTO);
        let text = render(&Builds { interval: Duration::from_secs(1) }.collect(&ctx).unwrap());
        assert!(text.contains("nix_builds_active{origin=\"local\",client=\"burke\"} 1\n"), "{text}");
        assert!(text.contains("nix_build_oldest_start_timestamp_seconds "));
    }

    #[test]
    fn nothing_without_builds_or_uid_rule() {
        let d = tempfile::tempdir().unwrap();
        let ctx = Ctx::for_test(setup(d.path()), "");
        let out = Builds { interval: Duration::from_secs(1) }.collect(&ctx).unwrap();
        assert_eq!(render(&out), "");
    }
}
```

- [ ] **Step 3: Register both in `src/collectors/mod.rs`**

Replace the module list and `all` with:

```rust
mod builds;
mod daemon;
mod liveness;
mod profiles;
mod store;

pub use liveness::dead_paths;

use crate::collector::Collector;
use std::time::Duration;

/// Every collector. `base` is the fast interval; slower collectors use 4x.
pub fn all(base: Duration, liveness: Duration) -> Vec<Box<dyn Collector>> {
    let slow = base * 4;
    vec![
        Box::new(store::Store { interval: base }),
        Box::new(daemon::Daemon::new(base)),
        Box::new(builds::Builds { interval: base }),
        Box::new(profiles::Profiles { interval: slow }),
        Box::new(liveness::Liveness { interval: liveness }),
    ]
}
```

- [ ] **Step 4: Test, fmt, clippy, commit**

Run: `nix develop -c cargo test`
Expected: all tests pass.

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add daemon and builds collectors"
```

---

### Task 12: Slots, builders and upkeep collectors

**Files:**
- Create: `src/collectors/slots.rs`, `src/collectors/builders.rs`, `src/collectors/upkeep.rs`
- Modify: `src/collectors/mod.rs`

- [ ] **Step 1: Write `src/collectors/slots.rs`**

```rust
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
        let held = locks::held_inodes(
            &fs::read_to_string(ctx.paths.proc_dir.join("locks")).unwrap_or_default(),
        );
        let files: Vec<(String, u64)> = fs::read_dir(ctx.paths.current_load())
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.file_name().into_string().ok()?, e.metadata().ok()?.ino())))
            .collect();
        let mut slots = Family::gauge("nix_builder_slots", "Build slots (maxJobs) of each remote builder.");
        let mut used = Family::gauge(
            "nix_builds_offloaded",
            "Builds this host is running on each remote builder.",
        );
        for m in machines.iter().filter(|m| !m.is_local()) {
            let prefix = m.slot_prefix();
            let n = files
                .iter()
                .filter(|(name, ino)| {
                    name.strip_prefix(&prefix)
                        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
                        && held.contains(ino)
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
        let ino = |f: &str| fs::metadata(load.join(f)).unwrap().ino();
        write(
            &root.join("proc/locks"),
            &format!(
                "1: FLOCK  ADVISORY  WRITE 819892 00:2a:{} 0 EOF\n2: FLOCK  ADVISORY  WRITE 1 00:2a:{} 0 EOF\n",
                ino("ssh-ng:__remotebuild@juicy-j-1"),
                ino("ssh-ng:__remotebuild@juicy-j.upload-lock")
            ),
        );
        let ctx = Ctx::for_test(paths, &format!("builders = @{}\n", root.join("machines").display()));
        let text = render(&Slots { interval: Duration::from_secs(1) }.collect(&ctx).unwrap());
        assert!(text.contains("nix_builder_slots{machine=\"ssh-ng://remotebuild@juicy-j\"} 32\n"), "{text}");
        assert!(text.contains("nix_builds_offloaded{machine=\"ssh-ng://remotebuild@juicy-j\"} 1\n"));
        assert!(!text.contains("localhost"));
    }
}
```

- [ ] **Step 2: Write `src/collectors/builders.rs`**

```rust
use crate::cmd;
use crate::collector::{nix_command, Collector, Ctx};
use crate::machines;
use crate::metrics::Family;
use anyhow::Result;
use std::time::{Duration, Instant};

pub struct Builders {
    pub interval: Duration,
}

impl Collector for Builders {
    fn name(&self) -> &'static str {
        "builders"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let cfg = ctx.nix_config()?;
        let mut up = Family::gauge("nix_builder_up", "Whether the last ping of the remote builder succeeded.");
        let mut secs = Family::gauge("nix_builder_ping_seconds", "Duration of the last ping of the remote builder.");
        for m in machines::load(cfg.get("builders"), &ctx.extra_machines)
            .iter()
            .filter(|m| !m.is_local())
        {
            let start = Instant::now();
            let ok = cmd::status(
                nix_command("nix").args([
                    "--extra-experimental-features",
                    "nix-command",
                    "store",
                    "info",
                    "--store",
                    &m.ping_uri(),
                ]),
                Duration::from_secs(10),
            )
            .is_ok();
            up.push(&[("machine", &m.uri)], if ok { 1.0 } else { 0.0 });
            secs.push(&[("machine", &m.uri)], start.elapsed().as_secs_f64());
        }
        Ok(vec![up, secs])
    }
}
```

No unit test: it only runs `nix store info`. Task 15 checks `nix_builder_up` on the test client.

- [ ] **Step 3: Write `src/collectors/upkeep.rs`**

```rust
use crate::cmd;
use crate::collector::{Collector, Ctx};
use crate::metrics::Family;
use crate::upkeep_log::{parse_log, parse_show, unix_ts, RunLog};
use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

pub struct Upkeep {
    pub interval: Duration,
}

pub struct Run {
    pub unit: String,
    pub props: HashMap<String, String>,
    pub log: RunLog,
}

/// Metrics for the last run of each unit. Units that are not loaded or have
/// never run are skipped.
pub fn families(runs: &[Run]) -> Vec<Family> {
    let mut start = Family::gauge("nix_upkeep_last_start_timestamp_seconds", "When the unit last started.");
    let mut success = Family::gauge("nix_upkeep_last_success", "Whether the unit's last run succeeded.");
    let mut duration = Family::gauge("nix_upkeep_last_duration_seconds", "How long the unit's last run took.");
    let mut freed = Family::gauge("nix_upkeep_last_freed_bytes", "Bytes freed by the unit's last run.");
    let mut deleted = Family::gauge("nix_upkeep_last_deleted_paths", "Store paths deleted by the unit's last GC.");
    let mut saved = Family::gauge(
        "nix_store_hardlink_saved_bytes",
        "Space saved by hard-linking identical files, as of the last GC.",
    );
    for r in runs {
        if r.props.get("LoadState").map(String::as_str) != Some("loaded") {
            continue;
        }
        let Some(s) = unix_ts(r.props.get("ExecMainStartTimestamp")) else {
            continue;
        };
        let l = [("unit", r.unit.as_str())];
        start.push(&l, s);
        let ok = r.props.get("Result").map(String::as_str) == Some("success");
        success.push(&l, if ok { 1.0 } else { 0.0 });
        if let Some(e) = unix_ts(r.props.get("ExecMainExitTimestamp")).filter(|e| *e >= s) {
            duration.push(&l, e - s);
        }
        if let Some(b) = r.log.freed_bytes {
            freed.push(&l, b);
        }
        if let Some(n) = r.log.deleted_paths {
            deleted.push(&l, n);
        }
        if let Some(b) = r.log.hardlink_saved_bytes {
            saved.samples.clear();
            saved.push(&[], b);
        }
    }
    vec![start, success, duration, freed, deleted, saved]
}

impl Collector for Upkeep {
    fn name(&self) -> &'static str {
        "upkeep"
    }

    fn interval(&self) -> Duration {
        self.interval
    }

    fn collect(&mut self, ctx: &Ctx) -> Result<Vec<Family>> {
        let mut runs = Vec::new();
        for unit in &ctx.upkeep_units {
            let show = cmd::output(
                Command::new("systemctl").args([
                    "show",
                    unit,
                    "--timestamp=unix",
                    "-p",
                    "LoadState",
                    "-p",
                    "Result",
                    "-p",
                    "InvocationID",
                    "-p",
                    "ExecMainStartTimestamp",
                    "-p",
                    "ExecMainExitTimestamp",
                ]),
                Duration::from_secs(10),
            )?;
            let props = parse_show(&show);
            let log = match props.get("InvocationID").filter(|i| !i.is_empty()) {
                Some(id) => parse_log(&cmd::output(
                    Command::new("journalctl").args([
                        "--no-pager",
                        "-o",
                        "cat",
                        &format!("_SYSTEMD_INVOCATION_ID={id}"),
                    ]),
                    Duration::from_secs(30),
                )?),
                None => RunLog::default(),
            };
            runs.push(Run {
                unit: unit.clone(),
                props,
                log,
            });
        }
        Ok(families(&runs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::render;

    #[test]
    fn reports_last_runs() {
        let gc = Run {
            unit: "nix-gc.service".into(),
            props: parse_show("LoadState=loaded\nResult=success\nExecMainStartTimestamp=@1000\nExecMainExitTimestamp=@1045\n"),
            log: parse_log("note: hard linking is currently saving 1.0 KiB\n3 store paths deleted, 2.0 KiB freed\n"),
        };
        let missing = Run {
            unit: "nope.service".into(),
            props: parse_show("LoadState=not-found\n"),
            log: RunLog::default(),
        };
        let never = Run {
            unit: "nix-optimise.service".into(),
            props: parse_show("LoadState=loaded\nResult=success\nExecMainStartTimestamp=\n"),
            log: RunLog::default(),
        };
        let text = render(&families(&[gc, missing, never]));
        assert!(text.contains("nix_upkeep_last_start_timestamp_seconds{unit=\"nix-gc.service\"} 1000\n"), "{text}");
        assert!(text.contains("nix_upkeep_last_success{unit=\"nix-gc.service\"} 1\n"));
        assert!(text.contains("nix_upkeep_last_duration_seconds{unit=\"nix-gc.service\"} 45\n"));
        assert!(text.contains("nix_upkeep_last_freed_bytes{unit=\"nix-gc.service\"} 2048\n"));
        assert!(text.contains("nix_upkeep_last_deleted_paths{unit=\"nix-gc.service\"} 3\n"));
        assert!(text.contains("nix_store_hardlink_saved_bytes 1024\n"));
        assert!(!text.contains("nope.service"));
        assert!(!text.contains("nix-optimise.service"));
    }
}
```

- [ ] **Step 4: Register in `src/collectors/mod.rs`**

```rust
mod builders;
mod builds;
mod daemon;
mod liveness;
mod profiles;
mod slots;
mod store;
mod upkeep;

pub use liveness::dead_paths;

use crate::collector::Collector;
use std::time::Duration;

/// Every collector. `base` is the fast interval; slower collectors use 4x.
pub fn all(base: Duration, liveness: Duration) -> Vec<Box<dyn Collector>> {
    let slow = base * 4;
    vec![
        Box::new(store::Store { interval: base }),
        Box::new(daemon::Daemon::new(base)),
        Box::new(builds::Builds { interval: base }),
        Box::new(slots::Slots { interval: base }),
        Box::new(builders::Builders { interval: slow }),
        Box::new(profiles::Profiles { interval: slow }),
        Box::new(upkeep::Upkeep { interval: slow }),
        Box::new(liveness::Liveness { interval: liveness }),
    ]
}
```

- [ ] **Step 5: Test, fmt, clippy, commit**

Run: `nix develop -c cargo test`
Expected: all tests pass.

```bash
nix develop -c cargo fmt && nix develop -c cargo clippy --all-targets -- -D warnings
git add src/
git commit -m "Add builder slots, builder ping and upkeep collectors"
```

---

### Task 13: `serve`, CLI wiring

**Files:**
- Create: `src/serve.rs`
- Modify: `src/main.rs` (replace whole file)

- [ ] **Step 1: Write `src/serve.rs`**

```rust
//! The unprivileged HTTP side: serves the snapshot file.

use crate::collector::build_info;
use crate::metrics::{render, Family};
use anyhow::{anyhow, Result};
use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// The snapshot plus its age, or just build info if there is no snapshot yet.
pub fn body(input: &Path, now: SystemTime) -> String {
    let text = fs::read_to_string(input);
    let modified = fs::metadata(input).and_then(|m| m.modified());
    match (text, modified) {
        (Ok(text), Ok(m)) => {
            let age = now.duration_since(m).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            text + &render(&[Family::gauge(
                "nix_exporter_snapshot_age_seconds",
                "Seconds since the collector last wrote its snapshot.",
            )
            .with(&[], age)])
        }
        _ => render(&[build_info()]),
    }
}

pub fn serve(listen: &str, input: &Path) -> Result<()> {
    let server = tiny_http::Server::http(listen).map_err(|e| anyhow!("listening on {listen}: {e}"))?;
    let content_type =
        tiny_http::Header::from_bytes("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            .expect("valid header");
    for request in server.incoming_requests() {
        let path = request.url().split('?').next().unwrap_or("");
        let response = if path == "/metrics" {
            tiny_http::Response::from_string(body(input, SystemTime::now()))
                .with_header(content_type.clone())
        } else {
            tiny_http::Response::from_string("nix-exporter: metrics are at /metrics\n")
                .with_status_code(404)
        };
        let _ = request.respond(response);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn appends_age_to_snapshot() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("metrics.prom");
        fs::write(&f, "nix_store_valid_paths 3\n").unwrap();
        let later = fs::metadata(&f).unwrap().modified().unwrap() + Duration::from_secs(5);
        let text = body(&f, later);
        assert!(text.starts_with("nix_store_valid_paths 3\n"));
        assert!(text.contains("nix_exporter_snapshot_age_seconds 5\n"), "{text}");
    }

    #[test]
    fn missing_snapshot_serves_build_info() {
        let text = body(Path::new("/nonexistent/metrics.prom"), SystemTime::now());
        assert!(text.contains("nix_exporter_build_info{version=\"0.1.0\"} 1\n"));
        assert!(!text.contains("snapshot_age"));
    }
}
```

- [ ] **Step 2: Replace `src/main.rs`** (drops `#![allow(dead_code)]`)

```rust
mod active_builds;
mod attribution;
mod buildset;
mod cgroup;
mod cmd;
mod collector;
mod collectors;
mod liveness;
mod locks;
mod machines;
mod metrics;
mod nettcp;
mod nixconf;
mod paths;
mod procs;
mod profiles;
mod resolve;
mod serve;
mod storedb;
mod upkeep_log;
mod users;

#[cfg(test)]
mod testutil;

use clap::{Parser, Subcommand};
use collector::{Ctx, Snapshot};
use paths::Paths;
use resolve::Resolver;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_SNAPSHOT: &str = "/run/nix-exporter/metrics.prom";

#[derive(Parser)]
#[command(version, about = "Prometheus exporter for the Nix store and daemon")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Collect metrics (as root) and keep writing them to a snapshot file.
    Collect {
        #[arg(long, default_value = DEFAULT_SNAPSHOT)]
        output: PathBuf,
        /// Seconds between fast collector cycles; slow collectors use 4x.
        #[arg(long, default_value_t = 15)]
        base_interval: u64,
        /// Seconds between dead-path computations.
        #[arg(long, default_value_t = 3600)]
        liveness_interval: u64,
        /// Extra machines file to read builders from (repeatable).
        #[arg(long = "machines-file")]
        machines_files: Vec<PathBuf>,
        /// systemd unit whose last run to report (repeatable).
        #[arg(long = "upkeep-unit", default_values = ["nix-gc.service", "nix-optimise.service"])]
        upkeep_units: Vec<String>,
        /// Report remote clients by address instead of reverse DNS name.
        #[arg(long)]
        no_resolve_peers: bool,
    },
    /// Serve the snapshot file over HTTP (unprivileged).
    Serve {
        #[arg(long, default_value = "127.0.0.1:9944")]
        listen: String,
        #[arg(long, default_value = DEFAULT_SNAPSHOT)]
        input: PathBuf,
    },
    /// Print the store paths a garbage collection would delete, sorted.
    PrintDead,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Collect {
            output,
            base_interval,
            liveness_interval,
            machines_files,
            upkeep_units,
            no_resolve_peers,
        } => {
            let ctx = Ctx::new(
                Paths::default(),
                machines_files,
                upkeep_units,
                Resolver::new(!no_resolve_peers),
            );
            collector::run(
                collectors::all(
                    Duration::from_secs(base_interval),
                    Duration::from_secs(liveness_interval),
                ),
                Arc::new(ctx),
                Arc::new(Snapshot::new(output)),
            )
        }
        Cmd::Serve { listen, input } => serve::serve(&listen, &input),
        Cmd::PrintDead => {
            let ctx = Ctx::new(Paths::default(), Vec::new(), Vec::new(), Resolver::new(false));
            for p in collectors::dead_paths(&ctx)? {
                println!("{p}");
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 3: Test, clippy (now with no `dead_code` allowance), smoke-run**

Run: `nix develop -c cargo test && nix develop -c cargo clippy --all-targets -- -D warnings`
Expected: all tests pass, no warnings. If clippy reports dead code, delete the unused item rather than allowing it.

Smoke run on juicy-j without root (most collectors will fail on permissions; that is expected and shows the failure path):

```bash
nix develop -c cargo build
./target/debug/nix-exporter collect --output /tmp/claude-nix-exporter.prom --base-interval 2 --liveness-interval 30 & sleep 8; kill %1
grep -E '^nix_exporter_collector_success|^nix_store_valid_paths|^nix_daemon_up' /tmp/claude-nix-exporter.prom
```

Expected: a `nix_exporter_collector_success` line per collector (8 lines); `store` succeeds (db.sqlite is world-readable), `nix_daemon_up 1`. Then:

```bash
./target/debug/nix-exporter serve --listen 127.0.0.1:19944 --input /tmp/claude-nix-exporter.prom & sleep 1
curl -s 127.0.0.1:19944/metrics | tail -3; curl -s -o /dev/null -w '%{http_code}\n' 127.0.0.1:19944/; kill %1
```

Expected: the file content followed by `nix_exporter_snapshot_age_seconds`, and `404` for `/`.

- [ ] **Step 4: fmt, commit**

```bash
nix develop -c cargo fmt
git add src/
git commit -m "Add serve and wire the CLI"
```

---

### Task 14: Nix package and modules

**Files:**
- Create: `nix/package.nix`, `nix/module.nix`, `nix/nixos.nix`, `nix/system-manager.nix`
- Modify: `flake.nix` (add `packages`, `nixosModules`, `systemManagerModules`, `checks`)

- [ ] **Step 1: Write `nix/package.nix`**

```nix
{ craneLib }:
let
  common = {
    src = craneLib.cleanCargoSource ../.;
    strictDeps = true;
  };
  cargoArtifacts = craneLib.buildDepsOnly common;
in
craneLib.buildPackage (
  common
  // {
    inherit cargoArtifacts;
    meta = {
      description = "Prometheus exporter for the Nix store and daemon";
      mainProgram = "nix-exporter";
    };
  }
)
```

- [ ] **Step 2: Write `nix/module.nix`** (shared by NixOS and system-manager)

```nix
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.nix-exporter;
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    optionalAttrs
    types
    ;
  snapshot = "/run/nix-exporter/metrics.prom";
  listen =
    if lib.hasInfix ":" cfg.listenAddress then
      "[${cfg.listenAddress}]:${toString cfg.port}"
    else
      "${cfg.listenAddress}:${toString cfg.port}";
  bin = lib.getExe cfg.package;
in
{
  options.services.nix-exporter = {
    enable = mkEnableOption "the Prometheus exporter for the Nix store and daemon";
    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "The nix-exporter package.";
    };
    nixPackage = mkOption {
      type = types.package;
      default = pkgs.nix;
      description = "Nix used for `nix config show`, `nix-store --print-roots` and builder pings.";
    };
    listenAddress = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = "Address `serve` listens on.";
    };
    port = mkOption {
      type = types.port;
      default = 9944;
      description = "Port `serve` listens on.";
    };
    extraMachinesFiles = mkOption {
      type = types.listOf types.path;
      default = [ ];
      description = "Machines files to read builders from, besides the `builders` setting (e.g. Hydra's).";
    };
    upkeepUnits = mkOption {
      type = types.listOf types.str;
      default = [
        "nix-gc.service"
        "nix-optimise.service"
      ];
      description = "systemd units whose last run is reported.";
    };
    resolvePeers = mkOption {
      type = types.bool;
      default = true;
      description = "Report remote clients by reverse DNS name rather than address.";
    };
    baseInterval = mkOption {
      type = types.ints.positive;
      default = 15;
      description = "Seconds between fast collector cycles; slow collectors use 4x.";
    };
    livenessInterval = mkOption {
      type = types.ints.positive;
      default = 3600;
      description = "Seconds between dead-path computations.";
    };
    hardening = mkOption {
      type = types.bool;
      default = true;
      description = "systemd sandboxing for both units. Turn off only where the host cannot apply it, such as inside systemd-nspawn.";
    };
  };

  config = mkIf cfg.enable {
    systemd.services.nix-exporter-collect = {
      description = "nix-exporter: collect Nix store and daemon metrics";
      wantedBy = [ "multi-user.target" ];
      after = [ "nix-daemon.socket" ];
      path = [
        cfg.nixPackage
        pkgs.systemd
        pkgs.openssh
      ];
      environment = {
        NIX_REMOTE = "daemon";
        XDG_CACHE_HOME = "/var/cache/nix-exporter";
      };
      serviceConfig = {
        ExecStart = lib.escapeShellArgs (
          [
            bin
            "collect"
            "--output"
            snapshot
            "--base-interval"
            (toString cfg.baseInterval)
            "--liveness-interval"
            (toString cfg.livenessInterval)
          ]
          ++ lib.concatMap (f: [
            "--machines-file"
            "${f}"
          ]) cfg.extraMachinesFiles
          ++ lib.concatMap (u: [
            "--upkeep-unit"
            u
          ]) cfg.upkeepUnits
          ++ lib.optional (!cfg.resolvePeers) "--no-resolve-peers"
        );
        Restart = "always";
        RestartSec = 5;
        RuntimeDirectory = "nix-exporter";
        RuntimeDirectoryMode = "0755";
        RuntimeDirectoryPreserve = true;
        CacheDirectory = "nix-exporter";
      }
      // optionalAttrs cfg.hardening {
        ProtectSystem = "strict";
        ProtectHome = "read-only";
        PrivateTmp = true;
        NoNewPrivileges = true;
        # SQLite readers of a WAL database map and lock db.sqlite-shm.
        ReadWritePaths = [ "/nix/var/nix/db" ];
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
          "AF_NETLINK"
        ];
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        LockPersonality = true;
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
      };
    };

    systemd.services.nix-exporter-serve = {
      description = "nix-exporter: serve metrics over HTTP";
      wantedBy = [ "multi-user.target" ];
      after = [ "nix-exporter-collect.service" ];
      serviceConfig = {
        ExecStart = lib.escapeShellArgs [
          bin
          "serve"
          "--listen"
          listen
          "--input"
          snapshot
        ];
        Restart = "always";
        RestartSec = 5;
      }
      // (
        if cfg.hardening then
          {
            DynamicUser = true;
            CapabilityBoundingSet = "";
            ProtectSystem = "strict";
            ProtectHome = true;
            PrivateTmp = true;
            PrivateDevices = true;
            NoNewPrivileges = true;
            RestrictAddressFamilies = [
              "AF_INET"
              "AF_INET6"
            ];
            ProtectKernelTunables = true;
            ProtectKernelModules = true;
            ProtectControlGroups = true;
            ProtectProc = "invisible";
            LockPersonality = true;
            MemoryDenyWriteExecute = true;
            RestrictRealtime = true;
            SystemCallArchitectures = "native";
            SystemCallFilter = [ "@system-service" ];
          }
        else
          { User = "nobody"; }
      );
    };
  };
}
```

- [ ] **Step 3: Write `nix/nixos.nix`**

```nix
{ self }:
{ config, lib, ... }:
let
  cfg = config.services.nix-exporter;
in
{
  imports = [ (import ./module.nix { inherit self; }) ];

  options.services.nix-exporter.firewallInterfaces = lib.mkOption {
    type = lib.types.listOf lib.types.str;
    default = [ ];
    example = [ "tailscale0" ];
    description = "Interfaces on which to open the port in the NixOS firewall.";
  };

  config = lib.mkIf cfg.enable {
    services.nix-exporter.nixPackage = lib.mkDefault config.nix.package;
    networking.firewall.interfaces = lib.genAttrs cfg.firewallInterfaces (_: {
      allowedTCPPorts = [ cfg.port ];
    });
  };
}
```

- [ ] **Step 4: Write `nix/system-manager.nix`**

```nix
# Not tested beyond evaluation; see the README.
{ self }:
{
  imports = [ (import ./module.nix { inherit self; }) ];
}
```

- [ ] **Step 5: Add outputs to `flake.nix`**

Inside the outputs attrset (next to `devShells`), add:

```nix
      packages = forEachLinuxSystem (
        { pkgs, ... }:
        {
          default = pkgs.callPackage ./nix/package.nix { craneLib = inputs.crane.mkLib pkgs; };
        }
      );

      nixosModules.default = import ./nix/nixos.nix { inherit self; };
      systemManagerModules.default = import ./nix/system-manager.nix { inherit self; };

      checks = forEachLinuxSystem (
        { system, ... }:
        {
          package = self.packages.${system}.default;
          system-manager = inputs.system-manager.lib.makeSystemConfig {
            modules = [
              self.systemManagerModules.default
              {
                nixpkgs.hostPlatform = system;
                services.nix-exporter.enable = true;
              }
            ];
          };
        }
      );
```

- [ ] **Step 6: Build and evaluate**

Run: `git add nix flake.nix && nix build .#default && ./result/bin/nix-exporter --version`
Expected: crane builds and runs the unit tests in the build; prints `nix-exporter 0.1.0`.

Run: `nix build .#checks.x86_64-linux.system-manager --dry-run`
Expected: evaluates without error. If system-manager rejects an option used in `nix/module.nix` (for example `path` or `environment` on a service), keep the option for NixOS by moving it into `nix/nixos.nix` and record the limitation for the README; do not drop it from NixOS.

Run: `nix flake check --no-build`
Expected: no evaluation errors.

- [ ] **Step 7: Commit**

```bash
nix fmt
git add nix flake.nix flake.lock
git commit -m "Package with crane; add NixOS and system-manager modules"
```

---

### Task 15: NixOS test (QEMU)

**Files:**
- Create: `nix/test.nix`
- Modify: `flake.nix` (add `vm` check)

- [ ] **Step 1: Write `nix/test.nix`**

```nix
{
  pkgs,
  self,
  backend,
}:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  sshKeys = import "${pkgs.path}/nixos/tests/ssh-keys.nix" pkgs;
  exporter = self.packages.${system}.default;

  # A build that takes `seconds`. builtins.storePath gives the derivation
  # bash and coreutils as inputs, so the sandbox has them.
  probe = pkgs.writeText "probe.nix" ''
    { name, seconds ? 30, uidRange ? false }:
    derivation ({
      inherit name;
      system = "${system}";
      builder = "''${builtins.storePath ${pkgs.bash}}/bin/bash";
      args = [ "-c" "''${builtins.storePath ${pkgs.coreutils}}/bin/sleep ''${toString seconds}; echo done > $out" ];
    } // (if uidRange then { requiredSystemFeatures = [ "uid-range" ]; } else { }))
  '';

  common = {
    imports = [ self.nixosModules.default ];
    services.nix-exporter = {
      enable = true;
      listenAddress = "0.0.0.0";
      baseInterval = 2;
      livenessInterval = 10;
      hardening = backend == "vm";
    };
    nix.settings.experimental-features = [ "nix-command" ];
    environment.systemPackages = [
      pkgs.curl
      pkgs.prometheus
      exporter
    ];
    environment.etc."probe.nix".source = probe;
    system.extraDependencies = [
      pkgs.bash
      pkgs.coreutils
    ];
    networking.firewall.enable = false;
    users.users.alice.isNormalUser = true;
  };

  vmOnly = lib.optionalAttrs (backend == "vm") {
    virtualisation.writableStore = true;
    virtualisation.memorySize = 2048;
  };

  builder = {
    imports = [
      common
      vmOnly
    ];
    services.openssh.enable = true;
    users.users.remotebuild = {
      isNormalUser = true;
      openssh.authorizedKeys.keys = [ sshKeys.snakeOilPublicKey ];
    };
    nix.settings = {
      trusted-users = [
        "root"
        "remotebuild"
      ];
      auto-allocate-uids = true;
      experimental-features = [
        "auto-allocate-uids"
        "cgroups"
      ];
      system-features = [
        "uid-range"
        "nixos-test"
        "big-parallel"
        "benchmark"
      ];
    };
    nix.gc.automatic = true;
    nix.optimise.automatic = true;
  };

  client = {
    imports = [
      common
      vmOnly
    ];
    nix.distributedBuilds = true;
    nix.buildMachines = [
      {
        hostName = "builder";
        protocol = "ssh-ng";
        sshUser = "remotebuild";
        sshKey = "/root/.ssh/id_snakeoil";
        inherit system;
        maxJobs = 2;
      }
    ];
    programs.ssh.extraConfig = ''
      Host builder
        StrictHostKeyChecking no
        UserKnownHostsFile /dev/null
    '';
  };

  machines = { inherit builder client; };
in
pkgs.testers.runNixOSTest (
  {
    name = "nix-exporter-${backend}";
    testScript = ''
      import time

      def scrape(m):
          return m.succeed("curl -sf http://127.0.0.1:9944/metrics")

      def value(text, series):
          for line in text.splitlines():
              if line.startswith(series + " "):
                  return float(line.rsplit(" ", 1)[1])
          return None

      def value_prefix(text, prefix):
          for line in text.splitlines():
              if line.startswith(prefix):
                  return float(line.rsplit(" ", 1)[1])
          return None

      def wait_for(m, check, what, timeout=180):
          deadline = time.time() + timeout
          while time.time() < deadline:
              text = scrape(m)
              if check(text):
                  return text
              time.sleep(1)
          raise Exception(f"{m.name}: timed out waiting for {what}; last scrape:\n{scrape(m)}")

      def start_build(m, name, user=None, extra=""):
          cmd = f"nix-build /etc/probe.nix --argstr name {name}-$RANDOM --no-out-link {extra} > /tmp/{name}.log 2>&1"
          if user:
              cmd = f"runuser -u {user} -- sh -c '{cmd}'"
          m.succeed(f"({cmd}) >/dev/null 2>&1 &")

      def wait_build_done(m, name):
          m.wait_until_succeeds(f"grep -q '^/nix/store/' /tmp/{name}.log", timeout=180)

      start_all()
      client.succeed("install -D -m600 ${sshKeys.snakeOilPrivateKey} /root/.ssh/id_snakeoil")
      for m in (builder, client):
          m.wait_for_unit("nix-exporter-collect.service")
          m.wait_for_unit("nix-exporter-serve.service")
          wait_for(m, lambda t: value(t, "nix_store_valid_paths") is not None, "store metrics")
      builder.wait_for_unit("sshd.service")

      cpu = 'nix_daemon_cgroup_cpu_seconds_total{mode="user"}'
      text = wait_for(builder, lambda t: value(t, cpu) is not None, "daemon cgroup counters")
      cpu_before = value(text, cpu)

      with subtest("local build through the daemon, auto-allocated build UIDs"):
          start_build(builder, "local", user="alice")
          wait_for(builder, lambda t: value(t, 'nix_builds_active{origin="local",client="alice"}') == 1, "alice's build")
          wait_for(builder, lambda t: (value(t, 'nix_daemon_memory_bytes{role="builds"}') or 0) > 0, "build memory")
          wait_build_done(builder, "local")

      with subtest("root building in-process with nixbld users"):
          start_build(client, "client-local", extra="--option builders \"\"")
          wait_for(client, lambda t: value(t, 'nix_builds_active{origin="local",client="root"}') == 1, "root's build")
          wait_for(client, lambda t: (value(t, 'nix_daemon_memory_bytes{role="builds"}') or 0) > 0, "nixbld build memory")
          wait_build_done(client, "client-local")

      with subtest("uid-range build"):
          start_build(builder, "uidrange", user="alice", extra="--arg uidRange true")
          builder.wait_until_succeeds("ls -d /sys/fs/cgroup/system.slice/nix-daemon.service/nix-build-uid-*", timeout=60)
          wait_for(builder, lambda t: value(t, 'nix_builds_active{origin="local",client="alice"}') == 1, "uid-range build")
          wait_build_done(builder, "uidrange")

      with subtest("remote build over ssh-ng"):
          wait_for(client, lambda t: value(t, 'nix_builder_up{machine="ssh-ng://remotebuild@builder"}') == 1, "builder ping")
          start_build(client, "remote", extra="--max-jobs 0")
          wait_for(builder, lambda t: value_prefix(t, 'nix_builds_active{origin="remote",client="client') == 1, "remote build on builder")
          wait_for(builder, lambda t: (value_prefix(t, 'nix_daemon_remote_sessions{user="remotebuild",client="client') or 0) >= 1, "remote session")
          wait_for(client, lambda t: value(t, 'nix_builds_offloaded{machine="ssh-ng://remotebuild@builder"}') == 1, "offloaded slot")
          wait_build_done(client, "remote")

      with subtest("dead paths match nix-store --print-dead"):
          builder.succeed("nix-exporter print-dead | LC_ALL=C sort > /tmp/ours")
          builder.succeed("nix-store --gc --print-dead 2>/dev/null | LC_ALL=C sort > /tmp/theirs")
          builder.succeed("diff -u /tmp/theirs /tmp/ours")
          builder.succeed("test -s /tmp/ours")
          wait_for(builder, lambda t: (value(t, "nix_store_dead_paths") or 0) > 0, "dead path count")

      with subtest("upkeep units"):
          builder.succeed("systemctl start nix-gc.service")
          wait_for(builder, lambda t: value(t, 'nix_upkeep_last_success{unit="nix-gc.service"}') == 1, "gc run")
          wait_for(builder, lambda t: value(t, 'nix_upkeep_last_deleted_paths{unit="nix-gc.service"}') is not None, "gc deleted paths")
          builder.succeed("systemctl start nix-optimise.service")
          wait_for(builder, lambda t: value(t, 'nix_upkeep_last_freed_bytes{unit="nix-optimise.service"}') is not None, "optimise freed bytes")

      with subtest("cgroup counters, collector health, exposition format"):
          cpu_after = value(scrape(builder), cpu)
          assert cpu_after > cpu_before, f"{cpu_before} -> {cpu_after}"
          for m in (builder, client):
              m.succeed("curl -sf http://127.0.0.1:9944/metrics | promtool check metrics")
              for line in scrape(m).splitlines():
                  if line.startswith("nix_exporter_collector_success{"):
                      assert line.endswith(" 1"), f"{m.name}: {line}"
    '';
  }
  // (if backend == "vm" then { nodes = machines; } else { containers = machines; })
)
```

- [ ] **Step 2: Add the check to `flake.nix`**

In `checks`, merge an x86_64-only set:

```nix
      checks = forEachLinuxSystem (
        { pkgs, system }:
        {
          package = self.packages.${system}.default;
          system-manager = inputs.system-manager.lib.makeSystemConfig {
            modules = [
              self.systemManagerModules.default
              {
                nixpkgs.hostPlatform = system;
                services.nix-exporter.enable = true;
              }
            ];
          };
        }
        // lib.optionalAttrs (system == "x86_64-linux") {
          vm = import ./nix/test.nix {
            inherit pkgs self;
            backend = "vm";
          };
        }
      );
```

- [ ] **Step 3: Run the test**

Run: `git add nix flake.nix && nix build -L .#checks.x86_64-linux.vm`
Expected: all subtests pass. On failure read the scrape printed by `wait_for` and the unit logs (`builder.succeed("journalctl -u nix-exporter-collect")` in an interactive run: `nix run .#checks.x86_64-linux.vm.driverInteractive`). Fix the exporter or module, re-run. Known risk points, in order of likelihood:
  1. `ReadWritePaths`/`ProtectSystem` blocking a path the collector needs: the journal shows the failing collector's error.
  2. The probe derivation failing in the sandbox: read `/tmp/<name>.log` in the VM.
  3. The uid-range build not getting its own cgroup on upstream Nix: if `nix-build-uid-*` never appears, drop that one assertion (keep the build attribution assertion) and note it in the README.
  4. `print-dead` differing from `nix-store --print-dead`: the `diff -u` output names the paths; compare against Nix's GC semantics (keep-derivations / keep-outputs / temp roots) and fix `src/liveness.rs` with a unit test reproducing the case.

- [ ] **Step 4: Commit**

```bash
nix fmt
git add nix flake.nix
git commit -m "Add NixOS VM test"
```

---

### Task 16: Container variant of the test

**Files:**
- Modify: `flake.nix` (add `container` check), possibly `nix/test.nix`, `README.md` later

- [ ] **Step 1: Add the check** next to `vm` in the x86_64-only set:

```nix
          container = import ./nix/test.nix {
            inherit pkgs self;
            backend = "container";
          };
```

- [ ] **Step 2: Run it**

Run: `git add flake.nix && nix build -L .#checks.x86_64-linux.container`
The host (juicy-j) has `auto-allocate-uids`, `uid-range` and the `cgroups` feature, which the NixOS manual requires for container tests.

- [ ] **Step 3: Decide from the result**

- Passes: commit.
- Fails because Nix builds cannot run inside the containers (errors from the probe builds, e.g. sandbox or user-namespace setup), or for another container-only reason that cannot be fixed in `nix/test.nix` within a few attempts: remove the `container` check from `flake.nix`, keep `backend = "container"` support in `nix/test.nix`, and record the exact error for the README (Task 17).
- Fails on an exporter assertion that the VM test passes: that is a real difference (e.g. cgroup layout); fix it if the exporter is wrong, otherwise make the assertion conditional on `backend` in the Python (`if "${backend}" == "vm":`) with a comment saying why.

- [ ] **Step 4: Commit**

```bash
nix fmt
git add flake.nix nix/test.nix
git commit -m "Add systemd-nspawn container variant of the NixOS test"
```

(or, if dropped: `git commit -m "Keep the container test backend but do not run it: <reason>"`)

---

### Task 17: README

**Files:**
- Create: `README.md`

- [ ] **Step 1: Write `README.md`** with these sections, filled from the spec and the results of Tasks 15–16:

1. **What it is**: one paragraph; the two units (`nix-exporter-collect` as root with no listener, writes `/run/nix-exporter/metrics.prom`; `nix-exporter-serve` unprivileged, serves it).
2. **Usage (NixOS)**: flake input + module, e.g.

   ```nix
   inputs.nix-exporter.url = "github:burk3/nix-exporter";
   # in a NixOS configuration
   imports = [ inputs.nix-exporter.nixosModules.default ];
   services.nix-exporter = {
     enable = true;
     listenAddress = "0.0.0.0";
     firewallInterfaces = [ "tailscale0" ];
     extraMachinesFiles = config.services.hydra.buildMachinesFiles; # if Hydra runs here
   };
   ```

   and the options table from the spec (`listenAddress`, `port` 9944, `firewallInterfaces`, `extraMachinesFiles`, `upkeepUnits`, `resolvePeers`, `baseInterval`, `livenessInterval`, `hardening`, `nixPackage`).
3. **system-manager**: `systemManagerModules.default`; "Not tested on system-manager beyond an evaluation check."
4. **Metrics**: the metric tables from the spec, as implemented.
5. **How builds are attributed**: short version of the spec's algorithm; Determinate's `active-builds/` vs. the process fallback on upstream Nix.
6. **Caveats**:
   - `nix-store --gc --print-roots` (run by the liveness and profiles collectors) deletes stale `gcroots/auto` links, as a GC does.
   - `ReadWritePaths=/nix/var/nix/db` is needed because SQLite WAL readers map the `-shm` file; the exporter opens the DB read-only.
   - Builder pings run every `4 × baseInterval` and open an ssh connection to each remote builder.
   - The container test variant: passing, or the reason it is not run (from Task 16).
7. **Development**: `nix develop`, `cargo test`, `nix build .#checks.x86_64-linux.vm`.

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "Add README"
```

---

### Task 18: Deploy to juicy-j on an all-the-nix branch

**Files (in `/home/burke/src/all-the-nix`, via a worktree):**
- Modify: `flake.nix`, `flake.lock`, `systems/x86_64-linux/juicy-j/monitoring.nix`

The main checkout has an uncommitted, unrelated user change (UPS settings in `systems/x86_64-linux/juicy-j/default.nix`); do not touch it. Work in a separate worktree branched from `HEAD` (which includes the committed `auto-allocate-uids` / `cgroups` settings, commit `05ae865`).

- [ ] **Step 1: Create the worktree and branch**

```bash
cd /home/burke/src/all-the-nix
git worktree add ../all-the-nix-nix-exporter -b nix-exporter
cd ../all-the-nix-nix-exporter
```

- [ ] **Step 2: Add the flake input** in `flake.nix` `inputs` (after `mikrotik-exporter`):

```nix
    nix-exporter = {
      url = "git+file:///home/burke/src/nix-exporter";
      inputs.nixpkgs.follows = "nixpkgs";
    };
```

and add the module for juicy-j only, next to the `lofty305-wsl-nixos` entry:

```nix
        systems.hosts.juicy-j.modules = with inputs; [
          nix-exporter.nixosModules.default
        ];
```

- [ ] **Step 3: Enable it and scrape it** in `systems/x86_64-linux/juicy-j/monitoring.nix`:

After the node-exporter block:

```nix
  ### nix-exporter — Nix store and daemon metrics, local-only
  services.nix-exporter = {
    enable = true;
    listenAddress = "127.0.0.1";
    port = 9944;
    # Hydra's localhost builder lives in its own machines file.
    extraMachinesFiles = config.services.hydra.buildMachinesFiles;
  };
```

and in `services.victoriametrics.prometheusConfig.scrape_configs`, after the `node` job:

```nix
        {
          job_name = "nix";
          scrape_interval = "15s";
          static_configs = [ { targets = [ "127.0.0.1:9944" ]; } ];
        }
```

- [ ] **Step 4: Lock and build**

```bash
git add -A
nix flake lock --update-input nix-exporter
nix build .#nixosConfigurations.juicy-j.config.system.build.toplevel
```

Expected: builds. Do not switch; the user switches.

- [ ] **Step 5: Commit on the branch** (no push)

```bash
nix fmt
git add -A
git commit -m "juicy-j: run nix-exporter and scrape it"
```

- [ ] **Step 6: Report** the worktree path, branch name, and the command for the user: `sudo nixos-rebuild switch --flake /home/burke/src/all-the-nix-nix-exporter#juicy-j` (or `nh os switch /home/burke/src/all-the-nix-nix-exporter`). Note that the branch does not contain the uncommitted UPS change from the main checkout, so switching to it reverts that until it is committed.
