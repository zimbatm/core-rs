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
          index-proof-check = pkgs.rustPlatform.buildRustPackage {
            pname = "amber-core-index-proof-check";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--example"
              "index-proof-check"
            ];
            doCheck = false;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            installPhase = ''
              runHook preInstall
              install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/examples/index-proof-check $out/bin/index-proof-check
              wrapProgram $out/bin/index-proof-check --prefix PATH : ${
                pkgs.lib.makeBinPath [
                  pkgs.e2fsprogs
                  pkgs.util-linux
                ]
              }
              runHook postInstall
            '';
          };
          store-check = pkgs.rustPlatform.buildRustPackage {
            pname = "amber-core-store-check";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoTestFlags = [
              "--lib"
              "--all-features"
            ];
            installPhase = "mkdir -p $out";
          };
          indexed-liveness-bench = pkgs.rustPlatform.buildRustPackage {
            pname = "amber-core-indexed-liveness-bench";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--example"
              "indexed-liveness-profile"
            ];
            doCheck = false;
            installPhase = ''
              runHook preInstall
              install -Dm755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/examples/indexed-liveness-profile $out/bin/indexed-liveness-profile
              runHook postInstall
            '';
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
