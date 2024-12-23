{
  lib,
  rustPlatform,
  clang,
  openssl,
  libgcc,
  pkg-config,
  sqlite,
  autoPatchelfHook,
  stdenv
}:

rustPlatform.buildRustPackage {
    pname = "assistant_bot_rs";
    version = "0.1.0";

    src = ../../.;

    cargoLock = {
      lockFile = ../../Cargo.lock;
      # outputHashes = {
      #   "teloxide-0.13.0" = "sha256-GHI3zs0Tvw5HtipIG/xS26RNXyYdAOdgqZ6CRajdnio=";
      # };
    };

    nativeBuildInputs = [
      clang
      pkg-config
      autoPatchelfHook
    ];

    buildInputs = [
      openssl
    ];

    preBuild = ''
      # addAutoPatchelfSearchPath ${sqlite.dev}/lib
      addAutoPatchelfSearchPath ${libgcc}/lib
    '';
}
