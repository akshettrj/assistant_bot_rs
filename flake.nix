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
    in with pkgs;
    {

      devShells.default = mkShell {
        name = "assistant_bot_rs";

        nativeBuildInputs = [
          clang
          fenix.packages."${system}".stable.toolchain
          taplo
        ];
      };

    }
  );
}
