#!/usr/bin/env bash
# One-off investigation for the nix-exporter design. Run as root.
#
#   probe.sh builder [--dead] [--watch SECS]
#       On a build host (juicy-j, swaine). Records store/daemon facts, starts
#       local probe builds (as $PROBE_USER and as root), and traces where each
#       active build came from. Keeps watching for --watch seconds (default 120)
#       so remote builds started from another host get traced too.
#       --dead also times `nix-store --gc --print-dead` (holds the GC lock).
#
#   probe.sh watch [--watch SECS]
#       On a build host: quick DB/root checks, then only traces builds that
#       other hosts send (no inventory, no local probes).
#
#   probe.sh client
#       On a host that offloads (freddie-kane). Starts one probe build with
#       --max-jobs 0 and records current-load locks and the build-remote
#       processes while it runs. Start `watch` on the target host first.
#
# Writes everything to /tmp/nix-exporter-probe-<host>-<time>.txt.

set -u
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 1; }

MODE=${1:-}
shift || true
DEAD=0
WATCH=120
ITERS=${ITERS:-25000000} # roughly 30s of busy loop in the sandbox's /bin/sh
PROBE_USER=${PROBE_USER:-burke}
while [ $# -gt 0 ]; do
  case $1 in
    --dead) DEAD=1 ;;
    --watch) WATCH=$2; shift ;;
    *) echo "unknown arg $1" >&2; exit 1 ;;
  esac
  shift
done

OUT=/tmp/nix-exporter-probe-$(hostname)-$(date +%Y%m%d-%H%M%S).txt
exec > >(tee "$OUT") 2>&1
chmod 644 "$OUT"

WORK=$(mktemp -d)
chmod 755 "$WORK"
trap 'rm -rf "$WORK"' EXIT
cat > "$WORK/probe.nix" <<'EOF'
{ name, iters }:
derivation {
  inherit name;
  system = builtins.currentSystem;
  builder = "/bin/sh";
  args = [ "-c" "i=0; while [ $i -lt ${toString iters} ]; do i=$((i+1)); done; echo > $out" ];
}
EOF
chmod 644 "$WORK/probe.nix"

section() { printf '\n===== %s =====\n' "$*"; }
probe_name() { echo "exporter-probe-$1-$(hostname)-$(date +%s%N)"; }

# Process ancestry: pid, user, comm, args for the pid and every parent.
ancestry() {
  local p=$1
  while [ -n "$p" ] && [ "$p" != 0 ]; do
    ps -o pid=,ppid=,user=,comm=,args= -p "$p" | cut -c1-200
    p=$(ps -o ppid= -p "$p" | tr -d ' ')
    [ "$p" = 1 ] && { ps -o pid=,user=,comm= -p 1; break; }
  done
}

# For each unix socket held by a pid, show the socket and its peer's owner.
# ss -xpn columns: netid state recvq sendq laddr lino paddr pino process...
sockets_of() {
  local pid=$1 ino
  for fd in /proc/"$pid"/fd/*; do
    ino=$(readlink "$fd" 2>/dev/null | sed -n 's/^socket:\[\([0-9]*\)\]$/\1/p')
    [ -n "$ino" ] || continue
    awk -v ino="$ino" -v fd="${fd##*/}" '
      NR == FNR { if ($6 == ino) { peer = $8; print "fd " fd " sock " ino " local=" $5 " peer_ino=" $8 } ; next }
      peer != "" && $6 == peer { $1=$2=$3=$4=""; print "    peer side:" $0 }
    ' "$WORK/ss.txt" "$WORK/ss.txt"
    # TCP sockets (sshd holds the client connection)
    grep -F "pid=$pid," "$WORK/sstcp.txt" | sed 's/^/    tcp: /' | sort -u
  done | sort -u
}

trace_build() {
  local f=$1 nixpid
  section "active build $(basename "$f")"
  echo "--- raw active-builds file"
  cat "$f"; echo
  nixpid=$(basename "$f" | cut -d- -f1)
  ss -xpn > "$WORK/ss.txt"
  ss -tnp > "$WORK/sstcp.txt"
  echo "--- ancestry of nixPid $nixpid"
  ancestry "$nixpid"
  echo "--- sockets of nixPid $nixpid"
  sockets_of "$nixpid"
  # Follow the peers one hop further: whatever is on the other end of the
  # worker's sockets (nix client, determinate-nixd, nix-daemon --stdio).
  for peerpid in $(sockets_of "$nixpid" | grep -o 'pid=[0-9]*' | cut -d= -f2 | sort -u); do
    [ "$peerpid" = "$nixpid" ] && continue
    echo "--- peer pid $peerpid ancestry"
    ancestry "$peerpid"
    echo "--- peer pid $peerpid sockets"
    sockets_of "$peerpid" | head -40
    # if the peer is under sshd, show the sshd TCP connection
    for a in $(ancestry "$peerpid" | awk '$4 ~ /sshd/ {print $1}'); do
      echo "--- sshd $a tcp"
      grep -F "pid=$a," "$WORK/sstcp.txt"
    done
  done
}

inventory() {
  section "versions and config"
  hostname; uname -a; nix --version
  nix config show 2>/dev/null | grep -E '^(max-jobs|cores|builders|build-users-group|auto-allocate-uids|use-cgroups|sandbox|min-free|max-free|trusted-users|substituters|json-log-path) '
  # Hydra passes its machines files to the queue runner as NIX_REMOTE_SYSTEMS
  hydra_machines=$(systemctl show hydra-queue-runner.service -p Environment --value 2>/dev/null |
    tr ' ' '\n' | sed -n 's/^NIX_REMOTE_SYSTEMS=//p' | tr ':' ' ')
  for f in /etc/nix/machines $hydra_machines; do [ -f "$f" ] && { echo "--- $f"; cat "$f"; }; done

  section "daemon processes and sockets"
  ps -e --forest -o pid,ppid,user,etimes,rss,args | grep -E 'nix-daemon|determinate|hydra-queue' | grep -v grep | cut -c1-200
  ss -xlpn | grep -E 'nix|determinate'

  section "current-load slots vs /proc/locks"
  for f in /nix/var/nix/current-load/*; do
    ino=$(stat -c %i "$f")
    held=$(awk -v ino="$ino" '{ split($6, a, ":"); if (a[3] == ino) print $2, $3, $4, "pid=" $5 }' /proc/locks)
    echo "$(basename "$f") ino=$ino ${held:-free}"
  done

  section "store db"
  ls -la /nix/var/nix/db
  SQL="nix run nixpkgs#sqlite -- -readonly /nix/var/nix/db/db.sqlite"
  $SQL '.schema ValidPaths' '.schema Refs'
  time $SQL "select count(*), sum(narSize), sum(ultimate = 1), sum(coalesce(sigs, '') != ''), max(registrationTime) from ValidPaths;"
  $SQL 'pragma journal_mode;'

  section "gc roots"
  time nix-store --gc --print-roots > "$WORK/roots.txt"
  wc -l < "$WORK/roots.txt"
  sed -E 's/ -> .*//; s#^(/proc)/[0-9]+.*#\1/<pid>#; s#^(/nix/var/nix/gcroots/auto)/.*#\1/*#; s#^(/nix/var/nix/profiles)/.*#\1/*#; s#^/home/[^/]*/.*#/home/*#' "$WORK/roots.txt" | sort | uniq -c | sort -rn | head -20
  echo "stale auto roots: $(find /nix/var/nix/gcroots/auto -xtype l -exec sh -c '[ -e "$(readlink "$1")" ] || echo x' _ {} \; | wc -l)"

  section "profiles"
  ls -la /nix/var/nix/profiles /nix/var/nix/profiles/per-user/* 2>/dev/null | head -60

  section "upkeep journal"
  for u in nix-gc nix-optimise; do
    systemctl show "$u.service" -p ExecMainStartTimestamp,ExecMainExitTimestamp,Result,InvocationID --no-pager
    journalctl -u "$u.service" -n 15 --no-pager -o short-iso
  done
  echo "--- determinate-nixd gc mentions"
  journalctl -u nix-daemon.service --since -30d --no-pager -o short-iso | grep -iE 'garbage|gc |freed|disk' | tail -20

  if [ "$DEAD" = 1 ]; then
    section "print-dead timing"
    time (nice -n 19 ionice -c 3 nix-store --gc --print-dead 2>/dev/null | wc -l)
  fi
}

watch_loop() {
  echo "watching active-builds for ${WATCH}s; start 'probe.sh client' on another host now"
  declare -A seen
  local end=$((SECONDS + WATCH))
  while [ $SECONDS -lt $end ]; do
    for f in /nix/var/nix/active-builds/*; do
      [ -e "$f" ] || continue
      [ -n "${seen[$f]:-}" ] && continue
      seen[$f]=1
      sleep 1 # let the build settle so its processes exist
      trace_build "$f"
    done
    sleep 1
  done
}

case $MODE in
  builder)
    inventory
    section "starting local probe builds"
    runuser -u "$PROBE_USER" -- nix build --no-link --impure --builders '' -f "$WORK/probe.nix" \
      --argstr name "$(probe_name user)" --arg iters "$ITERS" > "$WORK/b-user.log" 2>&1 &
    nix build --no-link --impure --builders '' -f "$WORK/probe.nix" \
      --argstr name "$(probe_name root-auto)" --arg iters "$ITERS" > "$WORK/b-root.log" 2>&1 &
    nix build --store daemon --no-link --impure --builders '' -f "$WORK/probe.nix" \
      --argstr name "$(probe_name root-daemon)" --arg iters "$ITERS" > "$WORK/b-rootd.log" 2>&1 &
    watch_loop
    section "nix ps --json at end"
    nix ps --json
    wait
    section "probe build logs"
    tail -n 3 "$WORK"/b-*.log
    ;;
  watch)
    # Only traces builds started from elsewhere, plus the quick checks the
    # first run got wrong. Start this, then `probe.sh client` on the client.
    section "store db timing"
    SQL="nix run nixpkgs#sqlite -- -readonly /nix/var/nix/db/db.sqlite"
    $SQL "select 1;" >/dev/null # fetch sqlite before timing
    time $SQL "select count(*), sum(narSize), sum(ultimate = 1), sum(coalesce(sigs, '') != ''), max(registrationTime) from ValidPaths;"
    time $SQL "select count(*) from Refs;"
    section "gc roots by kind, excluding /proc"
    nix-store --gc --print-roots | grep -v "^/proc/" | sed -E "s/ -> .*//; s#/[^/]*\$##" | sort | uniq -c | sort -rn | head -30
    watch_loop
    ;;
  client)
    section "versions and config"
    hostname; nix --version; cat /etc/nix/machines 2>/dev/null
    section "starting offloaded probe build"
    nix build --no-link --impure --max-jobs 0 -f "$WORK/probe.nix" \
      --argstr name "$(probe_name client)" --arg iters "$ITERS" > "$WORK/b-client.log" 2>&1 &
    bpid=$!
    for i in 1 2 3 4 5 6; do
      sleep 5
      section "sample $i"
      for f in /nix/var/nix/current-load/*; do
        ino=$(stat -c %i "$f")
        held=$(awk -v ino="$ino" '{ split($6, a, ":"); if (a[3] == ino) print $2, $3, $4, "pid=" $5 }' /proc/locks)
        [ -n "$held" ] && echo "HELD $(basename "$f") ino=$ino $held"
      done
      ls -la /nix/var/nix/active-builds
      ps -e --forest -o pid,ppid,user,etimes,args | grep -E 'build-remote|nix-daemon|ssh .*nix-daemon' | grep -v grep | cut -c1-200
      kill -0 "$bpid" 2>/dev/null || break
    done
    wait
    section "probe build log"
    cat "$WORK/b-client.log"
    ;;
  *)
    sed -n '2,21p' "$0"; exit 1 ;;
esac

echo; echo "wrote $OUT"
