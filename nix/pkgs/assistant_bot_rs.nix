{
  rustPlatform,
  openssl,
  pkg-config,
  sqlite
}:

rustPlatform.buildRustPackage {
    pname = "assistant_bot_rs";
    version = "0.1.0";

    src = ../../.;

    cargoLock.lockFile = ../../Cargo.lock;

    nativeBuildInputs = [
      openssl.dev
      pkg-config
      sqlite
    ];
}
