# Crane-based build: dependencies are built once in their own derivation, so
# changing the crate's code doesn't rebuild them.
{
  lib,
  craneLib,
  # A toolchain whose rustfmt understands the unstable options of rustfmt.toml.
  fmtToolchain,
}:

let
  crate = craneLib.crateNameFromCargoToml { cargoToml = ../Cargo.toml; };

  # Rust sources, TOML files (incl. rustfmt.toml) and Cargo.lock only.
  src = craneLib.cleanCargoSource ../.;

  commonArgs = {
    inherit src;
    inherit (crate) pname version;
    strictDeps = true;
  };

  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  package = craneLib.buildPackage (commonArgs // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--locked --package ${crate.pname}";
    # The tests run in `checks.tests`.
    doCheck = false;

    meta = {
      description = "A personal Telegram assistant bot";
      license = lib.licenses.mit;
      mainProgram = crate.pname;
    };
  });
in
{
  inherit package;

  checks = {
    inherit package;

    clippy = craneLib.cargoClippy (commonArgs // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
    });

    tests = craneLib.cargoTest (commonArgs // {
      inherit cargoArtifacts;
      cargoTestExtraArgs = "--workspace";
    });

    rustfmt = (craneLib.overrideToolchain fmtToolchain).cargoFmt {
      inherit src;
      inherit (crate) pname version;
    };

    taplo = craneLib.taploFmt {
      inherit (crate) pname version;
      src = lib.sources.sourceFilesBySuffices src [ ".toml" ];
    };
  };
}
