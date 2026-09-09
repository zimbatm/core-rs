{
  description = "github.com/amber-store/core-rs";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    systems.url = "github:nix-systems/default";

  };

  outputs =
    {
      self,
      nixpkgs,
      systems,
      ...
    }@inputs:
    let
      eachSystem =
        f: nixpkgs.lib.genAttrs (import systems) (system: f system nixpkgs.legacyPackages.${system});
    in
    {

      formatter = eachSystem (
        system: pkgs:
        pkgs.writeShellApplication {
          name = "amber-core-fmt";
          runtimeInputs = [
            pkgs.cargo
            pkgs.rustfmt
            pkgs.nixfmt
          ];
          text = ''
            cargo fmt
            nixfmt flake.nix
          '';
        }
      );

      packages = eachSystem (
        system: pkgs: {
          store-check = pkgs.rustPlatform.buildRustPackage {
            pname = "amber-core-store-check";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoTestFlags = [
              "--lib"
              "packstore::"
            ];
            installPhase = "mkdir -p $out";
          };
          checksum-bench = pkgs.rustPlatform.buildRustPackage {
            pname = "amber-core-checksum-bench";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--example"
              "checksum-throughput"
            ];
            doCheck = false;
            installPhase = ''
              runHook preInstall
              install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/examples/checksum-throughput $out/bin/checksum-throughput
              runHook postInstall
            '';
          };
        }
      );

      devShells = eachSystem (
        system: pkgs: {
          default = pkgs.mkShell {
            hardeningDisable = [ "all" ];

            # go regenerates the golden vectors (tools/vectorgen)
            packages = with pkgs; [
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              go
            ];
          };
        }
      );
    };
}
