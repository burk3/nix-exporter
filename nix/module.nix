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
      description = "Seconds between fast collector cycles; profiles and upkeep use 4x.";
    };
    livenessInterval = mkOption {
      type = types.ints.positive;
      default = 3600;
      description = "Seconds between dead-path computations.";
    };
    pingInterval = mkOption {
      type = types.ints.positive;
      default = 300;
      description = "Seconds between ssh pings of each remote builder.";
    };
    pingBuilders = mkOption {
      type = types.bool;
      default = true;
      description = "Whether to ping remote builders over ssh (`nix store info`). Each ping is an ssh login to the builder.";
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
            "--ping-interval"
            (toString cfg.pingInterval)
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
          ++ lib.optional (!cfg.pingBuilders) "--no-ping-builders"
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
