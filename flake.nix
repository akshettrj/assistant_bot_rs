{
  description = "A personal Telegram assistant bot";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, fenix, crane }: flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = nixpkgs.legacyPackages.${system};
      fenixPkgs = fenix.packages.${system};

      # Single source of truth, shared with rustup users.
      rustToolchain = fenixPkgs.fromToolchainFile {
        file = ./rust-toolchain.toml;
        sha256 = "sha256-p8h3Sl/YRByZfZTAKXdsvF6xEenXKrXSVvpphmZENH4=";
      };
      # `rustfmt.toml` uses unstable options; pinned by flake.lock.
      nightlyRustfmt = fenixPkgs.latest.rustfmt;

      craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

      build = pkgs.callPackage ./nix/build.nix {
        inherit craneLib;
        fmtToolchain = fenixPkgs.combine [ rustToolchain nightlyRustfmt ];
      };
    in
    {
      packages.default = build.package;

      # One-off: fetch the local keys of Tuya devices (e.g. smart bulbs)
      # through a Smart Life QR login. See scripts/tuya_local_key.py.
      apps.tuya-local-key = {
        type = "app";
        program = pkgs.lib.getExe (pkgs.writeShellApplication {
          name = "tuya-local-key";
          runtimeInputs = [
            (pkgs.python3.withPackages (python: [
              python.qrcode
              python.tuya-device-sharing-sdk
            ]))
          ];
          text = ''exec python3 ${./scripts/tuya_local_key.py} "$@"'';
        });
      };

      # `nix flake check`: build, clippy, tests and formatting.
      checks = build.checks;

      devShells.default = craneLib.devShell {
        name = "assistant_bot_rs";
        checks = self.checks.${system};

        packages = [
          nightlyRustfmt
          pkgs.sea-orm-cli
          pkgs.sqlite
          pkgs.taplo
        ];
      };
    }
  );
}
