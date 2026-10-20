# nix-exporter design

A Prometheus exporter for the Nix store and the running Nix daemon, written in
Rust. It is built for always-on NixOS hosts that build and serve builds:

- **juicy-j**: x86_64 workstation, runs Hydra (localhost is its only Hydra
  builder), serves ssh-ng builds to freddie-kane, offloads aarch64 to
  nixbuild.net. Runs VictoriaMetrics and Grafana.
- **swaine**: aarch64 Orange Pi, a pure remote builder for juicy-j (including
  Hydra) and freddie-kane.

Both run Determinate Nix (3.23.3 / Nix 2.35.3 at time of writing). Upstream Nix
must also work, with Determinate-only sources treated as optional.

## Goals

- Answer, per host: how big is the store and how much could a GC free; are GC
  and optimise running and doing anything; what is building here right now and
  who asked for it; what has this host sent to other builders, and are those
  builders reachable.
- Never interfere with Nix: no writes to Nix state, no Nix locks taken, no
  expensive work on the scrape path.
- No root process listens on the network.

## Non-goals (v1)

- Hydra metrics. Hydra has its own endpoints; anything missing belongs in Hydra.
- Event metrics (build success/failure counts, build duration histograms,
  substitution volume). These need an event source such as Determinate's JSON
  log socket or a `post-build-hook`; that is phase 2.
- determinate-nixd's automatic GC runs. It logs only that it is enabled, so
  there is nothing stable to observe yet (phase 2).
- Filesystem free/used space. node-exporter already reports it; dashboards join
  on `node_filesystem_avail_bytes{mountpoint="/nix"}`.

## Architecture

One binary, two subcommands, two systemd units.

```
nix-exporter collect   root, no listening sockets
  collectors on independent timers -> merged snapshot
  writes /run/nix-exporter/metrics.prom atomically (write tmp, rename)
  mode 0644 (metrics only; nothing secret), owner root
        |
        | one-way: root never reads anything from serve
        v
nix-exporter serve     DynamicUser, no capabilities
  GET /metrics -> file contents + nix_exporter_snapshot_age_seconds
  listens on the configured address (tailscale0 on t11s hosts)
```

Privilege separation is one-way on purpose: a compromised `serve` can read one
metrics file and nothing else, and has no channel into the root process. The
root side's only inputs are local system state (process titles, store paths,
journal text, DNS answers), which it reads either way.

`collect` rewrites the snapshot whenever any collector finishes a cycle.
`serve` reads the file on every scrape; it holds no other state.

### Collectors

Each collector implements one trait: run on its interval, produce metric
families. Each one is isolated: its failure does not affect the others.

| collector | interval | sources |
|---|---|---|
| `store` | 15s | `db.sqlite` read-only |
| `daemon` | 15s | `/proc`, the daemon unit's cgroup v2 files |
| `builds` | 15s | `/nix/var/nix/active-builds/`, `/proc`, `/proc/net/tcp{,6}`, reverse DNS |
| `slots` | 15s | machines files, `/nix/var/nix/current-load/`, `/proc/locks` |
| `builders` | 60s | `nix store info --store <uri>` per machine, with timeout |
| `profiles` | 60s | GC roots list, profile symlinks, `db.sqlite` |
| `upkeep` | 60s | `systemctl show`, `journalctl` |
| `liveness` | 1h | `nix-store --gc --print-roots`, `db.sqlite` reachability walk |

Implementation stays small and synchronous: one OS thread per collector,
`rusqlite` (bundled SQLite, opened `SQLITE_OPEN_READ_ONLY`, `busy_timeout`
1s), `tiny_http` for `serve`, `serde_json`, `clap`, `dns-lookup`. `/proc`,
`/sys/fs/cgroup` and the text exposition format are handled by small
in-tree modules (each parser unit-tested), and systemd is queried with
`systemctl show` / `journalctl` subprocesses rather than D-Bus. Base
intervals scale from `--base-interval` (default 15s; 60s collectors use 4x)
so the NixOS test can run them every 2s.

All `nix` subprocesses run with `NIX_REMOTE=daemon`: under
`ProtectSystem=strict` a root `nix-store` could not open the local store
read-write, and through the daemon `--print-roots` is uncensored for root
and the stale-link cleanup happens in the daemon.

## Verified facts this design relies on

Collected on juicy-j and freddie-kane with `research/probe.sh` on 2026-10-19.

- **Store DB** is SQLite in WAL mode. 176,584 valid paths, 1.5M refs on
  juicy-j; aggregate queries over `ValidPaths` and a count over `Refs` each
  take under 0.3s, including process start-up. Columns used: `ValidPaths(id,
  path, registrationTime, narSize, ultimate, sigs)`, `Refs(referrer,
  reference)`. `ultimate = 1` marks paths built locally.
- **Active builds** (Determinate Nix only; see Builds for upstream): Nix
  writes one root-only JSON file per local build to
  `/nix/var/nix/active-builds/<nixPid>-<n>`, e.g.
  `{"type":"build","derivation":"…drv","nixPid":…,"mainPid":…,"mainUser":{"name":"nixbld1","uid":30001},"startTime":…,"clientPid":null,"clientUid":null,"cgroup":null}`.
  Only builds executed locally get an entry; builds offloaded to another
  machine do not. `clientPid`/`clientUid` are null on Determinate 3.23.3 even
  for direct daemon clients.
- **Daemon workers**: `determinate-nixd` starts the main `nix-daemon`, which
  forks one worker per connection. The worker owns the accepted socket
  directly (determinate-nixd does not proxy it), and Nix rewrites the worker's
  process title to `nix-daemon <client-pid> …`. The client PID was correct for
  every probe: local `nix` as burke, root `nix --store daemon`, and a remote
  ssh-ng client.
  This is upstream behaviour (`src/nix/unix/daemon.cc`: "For debugging, stuff
  the pid into argv[1]"): the PID is `strncpy`d over the original `argv[1]`
  (`--daemon` upstream, `--option` under Determinate), so it is NUL-padded
  and truncated to that argument's length. A daemon started with no
  arguments has no title to read.
- **Build users vary with daemon settings.** With `build-users-group =
  nixbld` builds run as `nixbld<N>`. With `auto-allocate-uids = true`
  (juicy-j since 2026-10-20) they run as nameless UIDs from `start-id`
  (872415232) up to `start-id + id-count`, and active-builds reports
  `"mainUser":{"name":null,"uid":872415232}`. Builds that require `uid-range`
  run in a user namespace and get their own cgroup,
  `<daemon unit cgroup>/nix-build-uid-<uid>`; active-builds then has
  `"cgroup"` set and `utime`/`stime` filled. No controllers are enabled in the
  unit's `cgroup.subtree_control`, so those child cgroups have `cpu.stat` but
  no `memory.*` files. The unit cgroup's own counters include them.
- **Root without the daemon**: `nix build` as root with the default store
  builds in-process; `nixPid` is the `nix` process itself, not a worker.
- **Remote ssh-ng builds**: the worker's client is `nix-daemon --stdio`
  running as the ssh user (`remotebuild`), whose parent is an `sshd-session`
  holding the TCP connection; the TCP peer address identifies the client host.
- **Offloaded builds**: each builder slot is a file in
  `/nix/var/nix/current-load/`, named after the store URI with `/` replaced by
  `_` plus `-<slot>` (e.g. `ssh-ng:__remotebuild@juicy-j.dab-ling.ts.net-0`).
  A slot in use is held as `FLOCK ADVISORY WRITE` by `nix __build-remote`,
  visible in `/proc/locks` by inode. `main-lock` and `*.upload-lock` are not
  slots. Stale slot files for machines no longer configured persist and must
  be ignored.
- **Machines files**: `/etc/nix/machines` (via the `builders` setting) and, on
  juicy-j, Hydra's file, passed to `hydra-queue-runner` as
  `NIX_REMOTE_SYSTEMS`.
- **GC roots**: `nix-store --gc --print-roots` takes 0.15s on juicy-j (4,085
  roots, mostly runtime roots under `/proc`). It deletes stale links in
  `gcroots/auto` as a side effect, as a GC would.
- **Dead paths**: `nix-store --gc --print-dead` took 49.6s of CPU (20,086
  dead paths) and holds the GC lock, so the exporter does not use it.
- **Profiles**: system profile at `/nix/var/nix/profiles/system-N-link`;
  home-manager profiles under `~/.local/state/nix/profiles`, not
  `/nix/var/nix/profiles/per-user`.
- **Upkeep journal lines** (parseable):
  `N store paths deleted, X KiB freed`;
  `note: hard linking is currently saving 71.2 GiB`;
  `2.2 GiB freed by hard-linking 618926 files`.
  Unit state (`ExecMainStartTimestamp`, `ExecMainExitTimestamp`, `Result`) is
  available from systemd.

## Metrics

Labels only take values from small bounded sets. Store paths and derivations
are never label values.

### Store (`store`)

| metric | type | meaning |
|---|---|---|
| `nix_store_valid_paths` | gauge | `count(*)` of `ValidPaths` |
| `nix_store_nar_bytes` | gauge | `sum(narSize)` |
| `nix_store_paths{origin="built"\|"substituted"}` | gauge | split on `ultimate` |
| `nix_store_signed_paths` | gauge | paths with a non-empty `sigs` |
| `nix_store_last_registration_timestamp_seconds` | gauge | `max(registrationTime)` |
| `nix_store_db_bytes` | gauge | size of `db.sqlite` plus `db.sqlite-wal` |

### Liveness (`liveness`)

| metric | type | meaning |
|---|---|---|
| `nix_store_dead_paths` | gauge | valid paths not reachable from any root |
| `nix_store_dead_bytes` | gauge | their summed `narSize` |
| `nix_store_gc_roots{kind}` | gauge | `kind` = `runtime`, `profile`, `auto`, `hydra`, `other` |
| `nix_store_gc_roots_stale` | gauge | `gcroots/auto` links whose target is gone |

Algorithm: first count stale roots with our own `readlink` pass over
`gcroots/auto` (before `--print-roots` deletes them). Then take root store
paths from `nix-store --gc --print-roots`, add temp roots from
`/nix/var/nix/temproots/`, load `Refs` into memory, and walk reachability. If
`keep-derivations` / `keep-outputs` are set, follow derivers and outputs as
Nix does. Every path not reached is dead.

### Profiles (`profiles`)

Profiles are discovered from the GC roots list: any root path matching
`<dir>/<name>-<N>-link`. Label `profile` is the profile's path without the
generation suffix, e.g. `/nix/var/nix/profiles/system`,
`/home/burke/.local/state/nix/profiles/home-manager`.

| metric | type |
|---|---|
| `nix_profile_generations{profile}` | gauge |
| `nix_profile_current_generation{profile}` | gauge |
| `nix_profile_current_timestamp_seconds{profile}` | gauge (link mtime) |
| `nix_profile_oldest_timestamp_seconds{profile}` | gauge |
| `nix_profile_closure_bytes{profile}` | gauge (current generation, `Refs` walk) |

### Daemon (`daemon`)

The main daemon is the `nix-daemon` process that is not a worker (its title is
not `nix-daemon <pid> …`). Workers are its `nix-daemon` children. The
`crashpad_handler` children are ignored.

| metric | type | meaning |
|---|---|---|
| `nix_daemon_up` | gauge | main daemon process found |
| `nix_daemon_info{version,variant}` | gauge = 1 | `variant` = `determinate` or `upstream` |
| `nix_daemon_start_time_seconds` | gauge | main daemon start time |
| `nix_daemon_connections` | gauge | worker count |
| `nix_daemon_remote_sessions{user,client}` | gauge | workers whose client is `nix-daemon --stdio` under sshd |
| `nix_daemon_memory_bytes{role}` | gauge | RSS; `role` = `daemon` (main + workers) or `builds` (build processes, see below) |
| `nix_daemon_max_jobs` | gauge | the `max-jobs` setting |

Build processes are identified without assuming a build user scheme: every
process descended from an active build's `mainPid`, plus any process whose
UID is a build UID (members of `build-users-group` when that is set, or
`[start-id, start-id + id-count)` when `auto-allocate-uids` is on; both read
from `nix config show`). The UID rule catches processes that escaped the
build's process tree and covers daemons without `active-builds/`.

Unit cgroup metrics come from the main daemon's cgroup, found through
`/proc/<pid>/cgroup` (cgroup v2 only), so they work whatever the unit is
called. On juicy-j that is `/sys/fs/cgroup/system.slice/nix-daemon.service`.
The cgroup holds the daemon, its workers and every build they start (Hydra's
and remote builds included), and usage of exited processes stays counted, so
these are true counters. They reset when the daemon restarts. A root
`nix build` that bypasses the daemon runs outside this cgroup and is not
counted. This needs only cgroup v2; it does not need `use-cgroups`.

| metric | type | source |
|---|---|---|
| `nix_daemon_cgroup_cpu_seconds_total{mode="user"\|"system"}` | counter | `cpu.stat` `user_usec`, `system_usec` |
| `nix_daemon_cgroup_memory_bytes` | gauge | `memory.current` |
| `nix_daemon_cgroup_memory_peak_bytes` | gauge | `memory.peak` |
| `nix_daemon_cgroup_io_bytes_total{direction="read"\|"write"}` | counter | `io.stat` `rbytes`, `wbytes` summed over devices |
| `nix_daemon_pressure_seconds_total{resource="cpu"\|"memory"\|"io",kind="some"\|"full"}` | counter | `<resource>.pressure` `total=` |

A missing controller file (e.g. memory accounting off) omits that metric only.
Without cgroup v2 the whole cgroup group is omitted.

Per-build cgroups (`use-cgroups = true`) are not used in v1. Reading them only
sees builds alive at sample time, and Nix deletes a build's cgroup when the
build ends, so totals per origin would miss short builds. See phase 2.

### Builds (`builds`)

| metric | type | meaning |
|---|---|---|
| `nix_builds_active{origin,client}` | gauge | builds executing on this host |
| `nix_build_oldest_start_timestamp_seconds` | gauge | oldest `startTime` among active builds |

Origin attribution for each active-builds entry:

1. Read `nixPid`'s `/proc/<pid>/cmdline`.
   - If the process is `nix` (not `nix-daemon`), the build runs in-process:
     `origin="local"`, `client=<user of nixPid>`.
   - If its title is `nix-daemon <client-pid> …`, continue with the client.
   - Otherwise `origin="unknown"`, `client=""`.
2. Classify the client process:
   - `nix-daemon --stdio` (ssh-ng) or `nix-store --serve` (legacy `ssh://`
     builders, e.g. Hydra machines files using `ssh://`): walk up to the nearest `sshd-session` ancestor,
     find its TCP socket inode in `/proc/<pid>/fd`, match it in
     `/proc/net/tcp` and `/proc/net/tcp6`, take the peer address, reverse
     resolve it (cached, falls back to the IP). `origin="remote"`,
     `client=<hostname or IP>`.
   - user is `hydra-queue-runner` (or an ancestor runs as it):
     `origin="hydra"`, `client="hydra"`.
   - anything else: `origin="local"`, `client=<user name>`. Build UIDs
     never appear here: the client is the process that asked for the build,
     not the build.
3. If the active-builds entry has a non-null `clientUid`/`clientPid`, use them
   instead of step 1.
4. Any lookup that fails because a process exited: `origin="unknown"`.

When `active-builds/` does not exist, builds are found from processes
instead: every process with a build UID (see Daemon) whose parent does not
have a build UID is the top of one build. Its parent is the `nixPid` for
attribution and its start time is the build's start time. This is the path
upstream Nix takes: nixpkgs' Nix 2.34.9 and 2.35.3 have no `active-builds/`
or `nix ps` (checked 2026-10-20); they are Determinate-only. With neither
`active-builds/` nor a build UID rule (no `build-users-group`, no
`auto-allocate-uids`) the family is absent and the collector still succeeds.

### Builder slots (`slots`) and builders (`builders`)

Machines come from the `builders` setting (read with `nix config show`, files
referenced as `@path` expanded) plus `--machines-file` arguments. The `machine`
label is the store URI from the machines entry.

| metric | type | meaning |
|---|---|---|
| `nix_builder_slots{machine}` | gauge | `maxJobs` from the machines entry |
| `nix_builds_offloaded{machine}` | gauge | slot files for that machine whose inode is in `/proc/locks` |
| `nix_builder_up{machine}` | gauge | last ping succeeded |
| `nix_builder_ping_seconds{machine}` | gauge | last ping duration |

Pings run `nix store info --store <uri>` with a 10s timeout, where `<uri>`
carries the entry's `ssh-key` and `base64-ssh-public-host-key` as store URI
parameters. An entry without a host key relies on root's existing
`known_hosts`; `/root` is read-only to the collector, so a new host key is a
failed ping, not a prompt. Machines whose URI is `localhost` or `local` are not pinged.
Slot file names are derived from the URI as Nix does (`/` replaced by `_`),
so `ssh://` and `ssh-ng://` machines both match.

### Upkeep (`upkeep`)

Units default to `nix-gc.service` and `nix-optimise.service` and are
configurable. A unit that does not exist produces no metrics.

| metric | type | meaning |
|---|---|---|
| `nix_upkeep_last_start_timestamp_seconds{unit}` | gauge | |
| `nix_upkeep_last_success{unit}` | gauge | `Result == success` |
| `nix_upkeep_last_duration_seconds{unit}` | gauge | exit minus start |
| `nix_upkeep_last_freed_bytes{unit}` | gauge | parsed from the last invocation's journal |
| `nix_upkeep_last_deleted_paths{unit}` | gauge | GC only |
| `nix_store_hardlink_saved_bytes` | gauge | from the last "currently saving" line |

Journal lines are read for the unit's last `InvocationID` only.

### Exporter

| metric | type |
|---|---|
| `nix_exporter_collector_success{collector}` | gauge |
| `nix_exporter_collector_duration_seconds{collector}` | gauge |
| `nix_exporter_collector_last_success_timestamp_seconds{collector}` | gauge |
| `nix_exporter_snapshot_age_seconds` | gauge (added by `serve`) |
| `nix_exporter_build_info{version}` | gauge = 1 |

## Error handling

- A collector that fails in a cycle publishes no metrics for that cycle, sets
  `success=0` and leaves `last_success_timestamp` unchanged. Old values are
  not served silently.
- Exception: `liveness` is slow by design and keeps serving its last result;
  its `last_success_timestamp` shows the age.
- Missing optional sources (no `active-builds/`, no upkeep unit, no machines
  file, upstream Nix) mean missing metrics, not failure.
- Processes, builds, lock files and sockets disappearing mid-scan are expected.
  The affected item is skipped; the collector does not fail.
- `serve` returns the last snapshot it can read. If the file does not exist
  yet, it returns only `nix_exporter_build_info` and
  `nix_exporter_snapshot_age_seconds` is absent.
- The exporter never takes a Nix lock, never writes Nix state, and never runs
  `--print-dead`. Its only side effect on Nix is that `--print-roots` removes
  stale `gcroots/auto` links; the README says so.

## Security

- `nix-exporter-collect.service`: `User=root`, `ProtectSystem=strict`,
  `ProtectHome=read-only`, `PrivateTmp=yes`, `NoNewPrivileges=yes`,
  `RuntimeDirectory=nix-exporter` and `CacheDirectory=nix-exporter`
  (`XDG_CACHE_HOME` for `nix`), plus `ReadWritePaths=/nix/var/nix/db`: SQLite
  readers of a WAL database need to map and lock `db.sqlite-shm`. The
  exporter only ever opens the DB read-only.
  `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK` (outbound only:
  builder pings, DNS, D-Bus), no listening sockets.
- `nix-exporter-serve.service`: `DynamicUser=yes`, `CapabilityBoundingSet=`
  (empty), `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`,
  `PrivateDevices=yes`, `RestrictAddressFamilies=AF_INET AF_INET6`.
- `hardening = false` drops these sandboxing settings and runs `serve` as
  `nobody` instead of a `DynamicUser` (both need namespaces that
  `systemd-nspawn` inside the Nix sandbox does not allow).
- Label values come from local data, some of it influenced by other users
  (process titles, PTR records). The exposition encoder escapes them; no other
  processing is done on them.

## Packaging

Flake outputs:

- `packages.<system>.default`: crane build, for `x86_64-linux` and
  `aarch64-linux`.
- `nixosModules.default`: imports the shared core module, adds the NixOS-only
  parts (`networking.firewall.interfaces.<iface>.allowedTCPPorts`).
- `systemManagerModules.default`: imports only the shared core module. Not a
  standard flake output name. The README states that system-manager support is
  untested beyond an evaluation check.
- `checks`: unit tests, NixOS VM test, `promtool check metrics`, system-manager
  evaluation.
- `devShells.default` (existing, extended with the Rust toolchain).

Module options under `services.nix-exporter`:

| option | default |
|---|---|
| `enable` | `false` |
| `listenAddress` | `"127.0.0.1"` |
| `port` | `9944` |
| `firewallInterfaces` (NixOS only) | `[ ]` |
| `extraMachinesFiles` | `[ ]` |
| `upkeepUnits` | `[ "nix-gc.service" "nix-optimise.service" ]` |
| `resolvePeers` | `true` |
| `baseInterval` | `15` (seconds) |
| `livenessInterval` | `3600` (seconds) |
| `nixPackage` | `config.nix.package` on NixOS, `pkgs.nix` elsewhere |
| `hardening` | `true` (systemd sandboxing on both units; set `false` only where the host cannot apply it, e.g. inside `systemd-nspawn`) |

The port is not registered on the Prometheus default-port wiki; check it is
still unallocated before release.

On juicy-j, `extraMachinesFiles` gets Hydra's machines file; swaine and
juicy-j open the port on `tailscale0`, and VictoriaMetrics on juicy-j gets a
`nix` scrape job for both hosts.

## Testing

- **Parser unit tests** with fixtures taken from the probe logs: machines
  files, slot file name to URI mapping, `/proc/locks`, worker titles,
  `/proc/net/tcp`, active-builds JSON, GC and optimise journal lines.
- **Store tests** against a generated SQLite DB with the real schema:
  aggregates, reachability walk, profile discovery and closure size.
- **NixOS test**, one definition built two ways:
  - `checks.x86_64-linux.vm`: QEMU nodes, `hardening = true`. This is the
    gate; it runs on juicy-j (`kvm`, `nixos-test`) and under Hydra.
  - `checks.x86_64-linux.container`: the same machines as `systemd-nspawn`
    containers with `hardening = false`, for fast iteration. Needs the host
    daemon settings from the NixOS manual (`auto-allocate-uids`, `uid-range`,
    `cgroups`), which juicy-j has. If Nix builds cannot run inside the test
    containers, this variant is dropped and the reason recorded in the README.

  Two machines, `builder` and `client`; `client` uses `builder` as an ssh-ng
  remote builder. `builder` runs with `auto-allocate-uids = true` and the
  `cgroups` feature, `client` with classic `nixbld` users, so both build user
  schemes are covered. The test machines run nixpkgs' upstream Nix, so the
  test exercises the process-based build discovery; the `active-builds/`
  path is covered by unit tests and by running on juicy-j (Determinate).
  Checks:
  - a local build on `builder` appears as `origin="local"`;
  - while a build runs, `nix_daemon_memory_bytes{role="builds"}` is non-zero
    on both machines (nameless auto-allocated UIDs and `nixbld` users);
  - a `uid-range` build on `builder` is attributed like any other build;
  - a build from `client` appears on `builder` as `origin="remote"`,
    `client=<client address>`, and on `client` as
    `nix_builds_offloaded{machine=…} 1`;
  - after running `nix-gc.service`, upkeep metrics are present;
  - `nix_daemon_cgroup_cpu_seconds_total` increases across a build;
  - `nix_store_dead_paths` equals the count from `nix-store --gc --print-dead`
    run in the test (exact set comparison via a debug flag that dumps the dead
    set).
- `promtool check metrics` over a scrape in the VM test.
- system-manager: a `makeSystemConfig` using `systemManagerModules.default`
  must evaluate.

## Phase 2 candidates

- Event metrics from Determinate's JSON log socket or a `post-build-hook`.
- Per-origin build CPU totals (`nix_builds_cpu_seconds_total{origin,client}`).
  With `use-cgroups = true`, Nix reads each build cgroup's CPU time before
  deleting it and returns it in the build result as `cpuUser`/`cpuSystem`,
  but only to the client; nothing on the builder records it. First check:
  does Determinate's JSON log carry those fields per build? If so, the counter
  is exact and `use-cgroups` becomes a documented prerequisite.
- determinate-nixd automatic GC runs.
- Hydra metrics, upstream in Hydra.
