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
