{
  description = "mzde: a desktop on river with the kwm window manager and the win-classic theme";

  inputs = {
    # The modules use the nixpkgs of the system. This input builds the
    # packages and the checks of this flake.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

    # river from the main branch with patches: the fork menduz/river.
    river = {
      url = "git+https://github.com/menduz/river.git?ref=main";
      flake = false;
    };
    # The window manager: the fork menduz/kwm of kewuaa/kwm.
    kwm = {
      url = "git+https://github.com/menduz/kwm.git?ref=master";
      flake = false;
    };
    # kwm starts kwim. kwim configures the input devices. The fork
    # menduz/kwim of kewuaa/kwim waits until river applies the settings.
    kwim = {
      url = "git+https://github.com/menduz/kwim.git?ref=master";
      flake = false;
    };
    # The screen locker: the fork menduz/waylock, with a password prompt.
    waylock = {
      url = "git+https://github.com/menduz/waylock.git?ref=master";
      flake = false;
    };
    # The Windows 9x theme of GTK, kwm and rofi.
    win-classic-theme = {
      url = "git+https://github.com/menduz/win-classic-theme.git?ref=main";
      flake = false;
    };
    # Only for the screenshots: they build a demo system with mzde.
    home-manager = {
      url = "github:nix-community/home-manager/release-26.05";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Terminal color themes from one color.
    mz-term-colors = {
      url = "git+https://github.com/menduz/mz-term-colors.git?ref=main";
      flake = false;
    };
    # The OpenSnitch fork: overlays.opensnitch builds it. Refer to
    # nix/opensnitch.nix.
    opensnitch = {
      url = "git+https://github.com/menduz/opensnitch.git?ref=fork";
      flake = false;
    };
  };

  outputs =
    { self, nixpkgs, ... }@inputs:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      packagesFor = pkgs: import ./pkgs { inherit pkgs inputs; };
      shots = import ./dev/screenshots.nix {
        pkgs = nixpkgs.legacyPackages.x86_64-linux;
        inherit (nixpkgs) lib;
        inherit inputs;
        nixosModule = self.nixosModules.default;
      };
    in
    {
      # The desktop. Import it in a NixOS configuration with Home Manager, and
      # set mzde.enable = true. Refer to README.md.
      nixosModules.default = import ./nix/nixos.nix inputs;
      nixosModules.mzde = self.nixosModules.default;
      # The named sandboxes and the `sandbox` command, without the desktop.
      # The desktop imports it. Refer to sandboxes/README.md.
      nixosModules.sandboxes = ./sandboxes/module.nix;

      # The OpenSnitch fork in place of opensnitch of nixpkgs (the daemon, the
      # UI, the eBPF modules). Refer to nix/opensnitch.nix.
      overlays.opensnitch = import ./nix/opensnitch.nix inputs;

      # The Nix library of mz-term-colors: `lib.termColors { inherit pkgs; }`.
      lib.termColors = import "${inputs.mz-term-colors}/lib";

      packages = forSystems (
        pkgs:
        let
          p = packagesFor pkgs;
        in
        {
          inherit (p)
            river
            kwm
            kwim
            waylock
            ;
          kwm-portal = pkgs.callPackage ./kwm-portal/nix/package.nix { };
          sandbox = pkgs.callPackage ./sandboxes/package.nix { };
        }
        // nixpkgs.lib.optionalAttrs (pkgs.stdenv.hostPlatform.system == "x86_64-linux") {
          inherit (shots) screenshots screenshot;
        }
      );

      checks.x86_64-linux =
        let
          pkgs = nixpkgs.legacyPackages.x86_64-linux;
          p = packagesFor pkgs;
        in
        {
          # The waylock prompt stays in the center after a change of the
          # output. It tests the patches of river and waylock.
          lock-prompt = pkgs.callPackage ./tests/lock-prompt.nix {
            inherit (p) river kwm waylock;
          };
          # kwim applies its settings when river is slow, as after a
          # resume. It tests the patch of kwim.
          kwim-apply = pkgs.callPackage ./tests/kwim-apply.nix {
            inherit (p) river kwm kwim;
          };
          # The build of the sandbox command with the tests of its Wayland
          # socket (sandboxes/tests/wayland.rs): a headless river of the fork
          # does not give the privileged globals to a sandbox, and the X
          # server of a sandbox works on that socket.
          sandbox-wayland = (pkgs.callPackage ./sandboxes/package.nix { }).overrideAttrs (o: {
            env = o.env // {
              SANDBOX_TEST_RIVER = nixpkgs.lib.getExe' p.river "river";
              SANDBOX_TEST_WAYLAND_INFO = nixpkgs.lib.getExe' pkgs.wayland-utils "wayland-info";
              SANDBOX_TEST_XDPYINFO = nixpkgs.lib.getExe pkgs.xdpyinfo;
            };
          });
          # The network modes, the home directory, the commands and the
          # labels of the sandboxes, with bubblewrap and gVisor.
          sandboxes = pkgs.testers.runNixOSTest (
            import ./sandboxes/test.nix {
              module = self.nixosModules.sandboxes;
              opensnitchOverlay = self.overlays.opensnitch;
            }
          );
        };

      # `nix run .#screenshots` writes screenshots/dark.png,
      # screenshots/light.png and screenshots/sandbox.png. Refer to
      # dev/screenshots.nix.
      apps.x86_64-linux.screenshots = {
        type = "app";
        program = nixpkgs.lib.getExe shots.update;
      };

      formatter = forSystems (pkgs: pkgs.nixfmt);
    };
}
