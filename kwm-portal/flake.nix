{
  description = "Desktop portal companion for kwm";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  # Used to evaluate the Home Manager module in checks.
  inputs.home-manager = {
    url = "github:nix-community/home-manager/release-26.05";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, home-manager, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forEachSystem = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      nixosModules = {
        default = ./nix/modules/nixos.nix;
        kwm-portal = ./nix/modules/nixos.nix;
      };
      homeManagerModules = {
        default = ./nix/modules/home-manager.nix;
        kwm-portal = ./nix/modules/home-manager.nix;
      };
      packages = forEachSystem (pkgs: {
        default = pkgs.callPackage ./nix/package.nix { };
        kwm-portal = pkgs.callPackage ./nix/package.nix { };
      });
      checks = forEachSystem (
        pkgs:
        {
          service = pkgs.callPackage ./nix/package.nix { };
        }
        // import ./nix/module-checks.nix { inherit pkgs nixpkgs home-manager; }
      );
      devShells = forEachSystem (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ (pkgs.callPackage ./nix/package.nix { }) ];
        };
      });
      formatter = forEachSystem (pkgs: pkgs.nixfmt-rfc-style);
    };
}
