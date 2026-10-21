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
