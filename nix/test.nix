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
      pingInterval = 4;
      hardening = backend == "vm";
    };
    nix.settings.experimental-features = [ "nix-command" ];
    environment.systemPackages = [
      pkgs.curl
      pkgs.prometheus.cli
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
    # nix-store --optimise fails with ESTALE renaming into directories of an
    # overlay whose lower layer is the host store over 9p; an erofs store
    # image as the lower layer supports it.
    virtualisation.useNixStoreImage = true;
    virtualisation.memorySize = 2048;
  };

  # The nspawn backend bind-mounts the build sandbox's /nix/store, owned by
  # a uid unmapped in the sandbox's user namespace, so Nix inside cannot
  # chown it, and there is no Nix database. At boot, put a tmpfs owned by the
  # container's root over /nix/store, bind the system closure back into it,
  # and register that closure, as qemu-vm does.
  containerOnly =
    { config, ... }:
    let
      closure = pkgs.closureInfo { rootPaths = [ config.system.build.toplevel ]; };
    in
    lib.optionalAttrs (backend == "container") {
      virtualisation.systemd-nspawn.options = [ "--bind-ro=${closure}:/nix/.closure" ];
      # The test script copies the key from the store; only the closure is bound.
      system.extraDependencies = [ sshKeys.snakeOilPrivateKey ];
      # Store paths keep the sandbox owner (unmapped, so "nobody"); ssh rejects
      # an Include of a file it does not consider owned by root.
      programs.ssh.systemd-ssh-proxy.enable = false;
      boot.nixStoreMountOpts = [
        "nodev"
        "nosuid"
      ];
      boot.postBootCommands = ''
        mount --make-rprivate /
        mkdir -p /nix/.new-store
        mount -t tmpfs -o mode=1775,nosuid,nodev tmpfs /nix/.new-store
        while read -r p; do
          n=/nix/.new-store/''${p#/nix/store/}
          if [ -L "$p" ]; then
            cp -P "$p" "$n"
          else
            if [ -d "$p" ]; then mkdir "$n"; else touch "$n"; fi
            mount --bind "$p" "$n"
          fi
        done < /nix/.closure/store-paths
        mount --move /nix/.new-store /nix/store
        ${lib.getExe' config.nix.package.out "nix-store"} --load-db < /nix/.closure/registration
      '';
    };

  builder = {
    imports = [
      common
      vmOnly
      containerOnly
    ];
    services.openssh.enable = true;
    # container-config.nix defaults to socket activation; the test waits for sshd.service.
    services.openssh.startWhenNeeded = false;
    users.users.remotebuild = {
      isNormalUser = true;
      openssh.authorizedKeys.keys = [ sshKeys.snakeOilPublicKey ];
      # nspawn test nodes run sshd without PAM, which refuses locked ("!")
      # accounts; "*" allows key login but no password.
      hashedPassword = lib.mkIf (backend == "container") "*";
    };
    nix.settings = {
      trusted-users = [
        "root"
        "remotebuild"
      ];
      experimental-features = [ "cgroups" ];
      system-features = [
        "nixos-test"
        "big-parallel"
        "benchmark"
      ];
    }
    # Auto-allocated build UIDs (872415232+) and uid-range builds do not fit
    # in the 65536 ids an nspawn container gets inside the build sandbox, so
    # the container backend builds with the classic nixbld users.
    // lib.optionalAttrs (backend == "vm") {
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
      containerOnly
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
          m.wait_for_open_port(9944)
          wait_for(m, lambda t: value(t, "nix_store_valid_paths") is not None, "store metrics")
      builder.wait_for_unit("sshd.service")

      cpu = 'nix_daemon_cgroup_cpu_seconds_total{mode="user"}'
      text = wait_for(builder, lambda t: value(t, cpu) is not None, "daemon cgroup counters")
      cpu_before = value(text, cpu)

      with subtest("local build through the daemon, ${
        if backend == "vm" then "auto-allocated build UIDs" else "nixbld build users"
      }"):
          start_build(builder, "local", user="alice")
          wait_for(builder, lambda t: value(t, 'nix_builds_active{origin="local",client="alice"}') == 1, "alice's build")
          wait_for(builder, lambda t: (value(t, 'nix_daemon_memory_bytes{role="builds"}') or 0) > 0, "build memory")
          wait_build_done(builder, "local")

      with subtest("root building in-process with nixbld users"):
          start_build(client, "client-local", extra="--option builders \"\"")
          wait_for(client, lambda t: value(t, 'nix_builds_active{origin="local",client="root"}') == 1, "root's build")
          wait_for(client, lambda t: (value(t, 'nix_daemon_memory_bytes{role="builds"}') or 0) > 0, "nixbld build memory")
          wait_build_done(client, "client-local")

      # uid-range needs 65536 ids of its own, more than an nspawn container has.
      if "${backend}" == "vm":
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
          # Nix also reports unregistered entries in /nix/store (the test VM's store
          # overlay shows paths it never registered); print-dead only knows valid ones.
          builder.succeed("nix-store --gc --print-dead 2>/dev/null | LC_ALL=C sort > /tmp/all")
          builder.succeed("xargs -r nix-store --check-validity --print-invalid < /tmp/all | LC_ALL=C sort > /tmp/invalid")
          builder.succeed("LC_ALL=C comm -23 /tmp/all /tmp/invalid > /tmp/theirs")
          builder.succeed("diff -u /tmp/theirs /tmp/ours")
          builder.succeed("test -s /tmp/ours")
          # The liveness collector runs every 10s here; its count should catch up.
          dead = int(builder.succeed("wc -l < /tmp/ours"))
          wait_for(builder, lambda t: value(t, "nix_store_dead_paths") == dead, f"nix_store_dead_paths == {dead}")
          wait_for(builder, lambda t: (value(t, "nix_store_dead_paths") or 0) > 0, "dead path count")

      with subtest("upkeep units"):
          builder.succeed("systemctl start nix-gc.service")
          wait_for(builder, lambda t: value(t, 'nix_upkeep_last_success{unit="nix-gc.service"}') == 1, "gc run")
          wait_for(builder, lambda t: value(t, 'nix_upkeep_last_deleted_paths{unit="nix-gc.service"}') is not None, "gc deleted paths")
          builder.succeed("systemctl start nix-optimise.service")
          if "${backend}" == "vm":
              wait_for(builder, lambda t: value(t, 'nix_upkeep_last_freed_bytes{unit="nix-optimise.service"}') is not None, "optimise freed bytes")
          else:
              # The container's store is per-path bind mounts, so optimise fails
              # hard-linking across them (EXDEV); check the exporter reports that.
              wait_for(builder, lambda t: value(t, 'nix_upkeep_last_success{unit="nix-optimise.service"}') == 0, "optimise failure")

      with subtest("profiles and roots"):
          # Test VMs boot without creating a system profile.
          builder.succeed("nix-env -p /nix/var/nix/profiles/system --set /run/current-system")
          wait_for(builder, lambda t: (value(t, 'nix_profile_generations{profile="/nix/var/nix/profiles/system"}') or 0) >= 1, "system profile")
          wait_for(builder, lambda t: (value(t, 'nix_store_gc_roots{kind="profile"}') or 0) >= 1, "profile roots")

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
