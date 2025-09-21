{ pkgs ? import <nixpkgs> {} }:

pkgs.rustPlatform.buildRustPackage {
  pname = "rust-cronwatcher";
  version = "0.1.0";

  src = ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  meta = with pkgs.lib; {
    description = "Rust-based cron-like task watcher and scheduler";
    license = licenses.mit;
    platforms = platforms.unix;
  };
}
