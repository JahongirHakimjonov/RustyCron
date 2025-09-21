{
  description = "Rust-based cron-like task watcher and scheduler";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        # --- Paket (binary qurish) ---
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "rust-cronwatcher";
          version = "0.1.0";

          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          meta = with pkgs.lib; {
            description = "Rust-based cron-like task watcher and scheduler";
            license = licenses.mit;
            maintainers = [ ];
            platforms = platforms.unix;
          };
        };

        # --- Developer muhiti (nix develop) ---
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pkgs.rustc
            pkgs.cargo
            pkgs.rust-analyzer
            pkgs.clippy
            pkgs.pkg-config
          ];
        };
      })
    # --- NixOS module: xizmat sifatida ishlatish ---
    // {
      nixosModules.default = { config, pkgs, lib, ... }: {
        options.services.rust-cronwatcher = {
          enable = lib.mkEnableOption "Rust cronwatcher task scheduler";
          configFile = lib.mkOption {
            type = lib.types.path;
            description = "Path to tasks.json configuration file";
            default = "/etc/rust-cronwatcher/tasks.json";
          };
        };

        config = lib.mkIf config.services.rust-cronwatcher.enable {
          systemd.services."rust-cronwatcher" = {
            description = "Rust Cronwatcher Service";
            wantedBy = [ "multi-user.target" ];
            after = [ "network.target" ];

            serviceConfig = {
              ExecStart = "${self.packages.${pkgs.system}.default}/bin/rust-cronwatcher --config ${config.services.rust-cronwatcher.configFile}";
              Restart = "on-failure";
            };
          };
        };
      };
    };
}
