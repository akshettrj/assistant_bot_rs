{
  description = "Flake for Telegram Assistant bot";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix.url = "github:nix-community/fenix";
  };

  outputs = { self, nixpkgs, flake-utils, fenix }: flake-utils.lib.eachDefaultSystem (system:
    let
      pkgs = import nixpkgs { inherit system; };
      rustToolchain = fenix.packages."${system}".latest.toolchain;
    in with pkgs;
    {

      devShells.default = mkShell {
        name = "assistant_bot_rs";

        buildInputs = [
          clang
          rustToolchain
          taplo
          sea-orm-cli
          openssl.dev
          openssl
          pkg-config
        ];

        PKG_CONFIG_PATH = "${openssl.dev}/lib/pkgconfig:${sqlite.dev}/lib/pkgconfig";
      };

      packages.default = callPackage ./nix/pkgs/assistant_bot_rs.nix {
        rustPlatform = makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
      };

    }
  );
}
