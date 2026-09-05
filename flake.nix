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
