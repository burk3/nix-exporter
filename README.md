# nix-exporter

A Prometheus exporter for the Nix store and daemon. It runs as two units:

- `nix-exporter-collect`, running as root, polls the store database, the
  Nix daemon's processes, cgroup, and config, and writes the result to
  `/run/nix-exporter/metrics.prom`. It has no listening socket.
- `nix-exporter-serve`, unprivileged, serves that file at `/metrics` over
  HTTP, adding `nix_exporter_snapshot_age_seconds`.

Splitting the two means the privileged half never talks to the network, and
the half that does (`serve`) never touches the store, the daemon, or `/proc`
of other users' processes.

## Usage (NixOS)

```nix
inputs.nix-exporter.url = "github:burk3/nix-exporter";
# or from FlakeHub: tagged releases
# inputs.nix-exporter.url = "https://flakehub.com/f/burk3/nix-exporter/0.1";
# or the latest master build (published as 0.0.<commit count>)
# inputs.nix-exporter.url = "https://flakehub.com/f/burk3/nix-exporter/0.0";

# in a NixOS configuration
imports = [ inputs.nix-exporter.nixosModules.default ];
services.nix-exporter = {
  enable = true;
  listenAddress = "0.0.0.0";
  firewallInterfaces = [ "tailscale0" ];
  extraMachinesFiles = config.services.hydra.buildMachinesFiles; # if Hydra runs here
};
```

Options under `services.nix-exporter`:

| option | default | meaning |
|---|---|---|
| `enable` | `false` | |
| `package` | the flake's own package | |
| `nixPackage` | `config.nix.package` (NixOS) / `pkgs.nix` | Nix used for `nix config show`, `nix-store --print-roots`, and builder pings |
| `listenAddress` | `"127.0.0.1"` | address `serve` listens on |
| `port` | `9944` | port `serve` listens on (not registered on the Prometheus default-port list yet) |
| `firewallInterfaces` (NixOS only) | `[ ]` | interfaces to open `port` on in the NixOS firewall |
| `extraMachinesFiles` | `[ ]` | extra machines files to read builders from, besides the `builders` setting (e.g. Hydra's) |
| `upkeepUnits` | `[ "nix-gc.service" "nix-optimise.service" ]` | systemd units whose last run is reported |
| `resolvePeers` | `true` | report remote clients by reverse DNS name rather than address |
| `baseInterval` | `15` (seconds) | fast collector cycle (store, daemon, builds, slots); the profiles and upkeep collectors run every 4x it, builders every `pingInterval`, and liveness every `livenessInterval` |
| `livenessInterval` | `3600` (seconds) | interval for the dead-path computation |
| `pingInterval` | `300` (seconds) | seconds between ssh pings of each remote builder |
| `pingBuilders` | `true` | ping remote builders at all; `false` leaves out the builders collector (no `nix_builder_up`/`nix_builder_ping_seconds`) |
| `hardening` | `true` | systemd sandboxing on both units; turn off only where the host can't apply it, e.g. inside `systemd-nspawn` |

## system-manager

`systemManagerModules.default` imports the same options and units, minus the
NixOS-only firewall option. Not tested on system-manager beyond an evaluation
check (the `system-manager` flake check evaluates and builds a config with
the module enabled; it is not booted or exercised).

## Metrics

### Store

| metric | meaning |
|---|---|
| `nix_store_valid_paths` | valid store paths |
| `nix_store_nar_bytes` | total NAR size of valid store paths |
| `nix_store_paths{origin="built"\|"substituted"}` | valid paths split by whether this host built them |
| `nix_store_signed_paths` | valid paths with at least one signature |
| `nix_store_last_registration_timestamp_seconds` | when the newest store path was registered |
| `nix_store_db_bytes` | size of `db.sqlite` plus `db.sqlite-wal` |

### Liveness

| metric | meaning |
|---|---|
| `nix_store_dead_paths` | valid paths not reachable from any GC root |
| `nix_store_dead_bytes` | their summed NAR size |
| `nix_store_gc_roots{kind}` | GC roots by kind: `runtime`, `profile`, `auto`, `hydra`, `other` |

This collector runs every `livenessInterval` seconds (default an hour).
When a run fails it keeps serving its last result, with
`nix_exporter_collector_success{collector="liveness"}` at 0, rather than
dropping these metrics.

There is no stale-root count. `nix-store --gc --print-roots`, which this
collector and the profiles collector both run, makes the daemon delete stale
`gcroots/auto` links and stale `temproots` files as a GC would, so a count of
them would almost always read 0.

### Profiles

Discovered from the GC roots list: any root matching `<dir>/<name>-<N>-link`.
`profile` is the profile's path with the generation suffix removed.

| metric | meaning |
|---|---|
| `nix_profile_generations{profile}` | generations kept |
| `nix_profile_current_generation{profile}` | the generation the profile symlink points at |
| `nix_profile_current_timestamp_seconds{profile}` | link mtime of the current generation |
| `nix_profile_oldest_timestamp_seconds{profile}` | link mtime of the oldest remaining generation |
| `nix_profile_closure_bytes{profile}` | NAR size of the current generation's closure |

### Daemon

| metric | meaning |
|---|---|
| `nix_daemon_up` | whether the main daemon process was found |
| `nix_daemon_info{version,variant}` | `=1`; `variant` is `determinate` or `upstream` |
| `nix_daemon_start_time_seconds` | when the daemon started |
| `nix_daemon_connections` | open daemon connections (worker process count) |
| `nix_daemon_remote_sessions{user,client}` | worker connections whose client is a remote ssh-ng/legacy-ssh peer |
| `nix_daemon_memory_bytes{role="daemon"\|"builds"}` | RSS of the daemon (main + workers) and of running build processes |
| `nix_daemon_max_jobs` | the `max-jobs` setting; absent when it is `auto` (or `nix config show` fails) |

Cgroup v2 metrics, read from the main daemon's own cgroup (via
`/proc/<pid>/cgroup`, so they work under whatever unit name wraps it):

| metric | meaning |
|---|---|
| `nix_daemon_cgroup_cpu_seconds_total{mode="user"\|"system"}` | cumulative CPU time, from `cpu.stat` |
| `nix_daemon_cgroup_memory_bytes` | current memory, from `memory.current` |
| `nix_daemon_cgroup_memory_peak_bytes` | peak memory, from `memory.peak` |
| `nix_daemon_cgroup_io_bytes_total{direction="read"\|"write"}` | cumulative block I/O, from `io.stat` |
| `nix_daemon_pressure_seconds_total{resource="cpu"\|"memory"\|"io",kind="some"\|"full"}` | PSI stall time |

These are counters because the cgroup holds the daemon, its workers, and
every build they start (including Hydra's and remote builds), and exited
processes stay counted. They reset when the daemon restarts. A build that
bypasses the daemon runs outside this cgroup and isn't counted. A missing
controller file (e.g. memory accounting off) omits just that metric; without
cgroup v2 the whole group is omitted.

### Builds

| metric | meaning |
|---|---|
| `nix_builds_active{origin,client}` | builds currently executing on this host |
| `nix_build_oldest_start_timestamp_seconds` | oldest start time among active builds |

When nothing is building, `nix_builds_active` still emits one series,
`{origin="local",client=""} 0`, so dashboards don't need `or vector(0)`.

### Builders and slots

Machines come from the `builders` config setting plus `--machines-file`
arguments (and `extraMachinesFiles`). `machine` is the store URI.
When the same URI appears more than once, the first entry's `maxJobs` wins.

| metric | meaning |
|---|---|
| `nix_builder_slots{machine}` | `maxJobs` from the machines entry |
| `nix_builds_offloaded{machine}` | slot lock files for that machine currently held |
| `nix_builder_up{machine}` | whether the last ping succeeded |
| `nix_builder_ping_seconds{machine}` | duration of the last ping |

`nix_builds_offloaded` counts the slot locks Nix's own build hook holds in
`/nix/var/nix/current-load/`. Hydra's queue runner does not use
`current-load/`, so a machine listed only in Hydra's machines file always
shows `nix_builds_offloaded` 0, however many Hydra builds it is running.

### Upkeep

One series per unit in `upkeepUnits` (default `nix-gc.service`,
`nix-optimise.service`); a unit that doesn't exist produces no metrics.

| metric | meaning |
|---|---|
| `nix_upkeep_last_start_timestamp_seconds{unit}` | |
| `nix_upkeep_last_success{unit}` | whether `Result == success` |
| `nix_upkeep_last_duration_seconds{unit}` | exit minus start |
| `nix_upkeep_last_freed_bytes{unit}` | parsed from the invocation's journal |
| `nix_upkeep_last_deleted_paths{unit}` | GC only |
| `nix_store_hardlink_saved_bytes` | from the "currently saving" line of the last GC run's journal |

### Exporter

| metric | meaning |
|---|---|
| `nix_exporter_collector_success{collector}` | whether the collector's last cycle succeeded |
| `nix_exporter_collector_duration_seconds{collector}` | how long the last cycle took |
| `nix_exporter_collector_last_success_timestamp_seconds{collector}` | when the collector last succeeded |
| `nix_exporter_snapshot_age_seconds` | seconds since `collect` last wrote the snapshot (added by `serve`) |
| `nix_exporter_build_info{version}` | `=1` |

A collector that panics is treated the same as one that returns an error:
that cycle is reported as failed (`nix_exporter_collector_success` goes to
`0`) rather than taking down its thread, so the other collectors keep
running. A subprocess that exceeds its timeout has its whole process group
killed, not just the direct child, so it can't leave orphaned grandchildren
behind.

## How builds are attributed

For each active build, the exporter walks from the process asking for the
build to who or what asked:

1. If the asking process is `nix` (not `nix-daemon`), the build is running
   in-process: `origin="local"`, `client=<user>`.
2. If it's a `nix-daemon <client-pid> …` worker, look at the client: an
   ssh-ng or legacy `nix-store --serve` connection (walked up to the nearest
   `sshd-session` ancestor, then matched to its TCP peer and reverse
   resolved) gives `origin="remote"`; a `hydra-queue-runner` client gives
   `origin="hydra"`; anything else gives `origin="local"` with that user.
3. Anything that can't be resolved because a process has already exited
   gives `origin="unknown"`.

Build processes themselves are found without assuming a particular build-user
scheme: every process descended from an active build's main PID, plus any
process whose UID falls in the configured build-UID range or group, counts.

Determinate Nix exposes `active-builds/` with this information directly.
Upstream Nix has no such directory — nixpkgs' Nix 2.34.9 and 2.35.3 ship
neither `active-builds/` nor `nix ps` — so on upstream the exporter instead
finds builds by walking processes for build UIDs and taking the first
non-build-UID ancestor as the asking process.

## Caveats

- Builder pings run `nix store info` against each non-local machine every
  `pingInterval` seconds (default 300). Each ping is an ssh login to the
  builder (e.g. eu.nixbuild.net), with whatever logging or cost that has on
  its side; set `pingBuilders = false` if that is unwanted.
- Machines entries literally named `localhost` are treated as local (Hydra's
  own meaning of that name) and get no slot or ping metrics. Query
  parameters in a machine URI aren't normalized the same way Nix names its
  slot lock files, so slot counts for URIs with several query parameters may
  read 0.
- `PrivateTmp` on the collector hides the host's real `/tmp`; a profile link
  living under `/tmp` won't be seen.
- `ReadWritePaths=/nix/var/nix/db` is needed even though the exporter only
  ever opens the database read-only: SQLite's WAL mode needs readers to be
  able to create or map the `-shm`/`-wal` files.
- `hardening = false` drops the sandboxing settings and runs `serve` as
  `nobody` instead of a `DynamicUser`, for hosts (such as inside
  `systemd-nspawn`) that can't grant the namespaces hardening needs.
- No license is declared yet.

## Testing

`nix flake check` runs four checks:

- `package`: builds the exporter.
- `system-manager`: evaluates and builds a system-manager configuration with
  the module enabled. Not run on booted system-manager.
- `vm`: a NixOS VM test (the gate) covering local builds through the daemon,
  auto-allocated build UIDs, uid-range builds, remote builds over ssh-ng,
  dead-path detection, upkeep units (including GC freeing space and optimise
  succeeding), profiles, GC roots, and cgroup counters. It boots with
  `virtualisation.useNixStoreImage = true`, because `nix-store --optimise`
  fails renaming across the 9p-backed store otherwise.
- `container`: the same scenarios run under `systemd-nspawn` instead of a VM.
  Because an nspawn container's user namespace only maps 65536 ids, the build
  user there can't be auto-allocated (`auto-allocate-uids`) or use
  `uid-range`; this check instead runs the builder with classic `nixbld`
  build users, so it does not cover auto-allocated UIDs or uid-range builds —
  the VM test covers both. It also asserts `nix-optimise` *fails* rather than
  checking freed bytes, since the container's store paths are separate bind
  mounts and hard-linking across them fails with `Invalid cross-device link`.
  Running this check on a real host needs the daemon settings the NixOS
  manual recommends for building inside containers: `auto-allocate-uids`,
  the `uid-range` system feature, and the `cgroups` experimental feature.

## Development

```
nix develop
cargo test
nix build .#checks.x86_64-linux.vm
```
