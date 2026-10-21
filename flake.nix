{
  description = "Prometheus exporter for the Nix store and daemon";

  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0";
    crane.url = "github:ipetkov/crane";
    system-manager = {
      # main requires nixpkgs 26.11; follow the release branch matching nixpkgs
      url = "github:numtide/system-manager/release-26.05";
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

      packages = forEachLinuxSystem (
        { pkgs, ... }:
        {
          default = pkgs.callPackage ./nix/package.nix { craneLib = inputs.crane.mkLib pkgs; };
        }
      );

      nixosModules.default = import ./nix/nixos.nix { inherit self; };
      systemManagerModules.default = import ./nix/system-manager.nix { inherit self; };

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
          container = import ./nix/test.nix {
            inherit pkgs self;
            backend = "container";
          };
        }
      );
    };
}
