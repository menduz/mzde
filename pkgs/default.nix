# The packages of mzwm, built with the given nixpkgs from the sources in the
# inputs of flake.nix.
{ pkgs, inputs }:
# The recipes of river, kwm and waylock are in their repositories (nix/), next
# to build.zig.zon and the hash of their Zig dependencies.
{
  river = pkgs.callPackage "${inputs.river}/nix" {
    riverSource = inputs.river;
    version = "0.6.0-dev-${inputs.river.shortRev or "dirty"}";
  };
  kwm = pkgs.callPackage "${inputs.kwm}/nix" {
    kwmSource = inputs.kwm;
    version = "0-unstable-${inputs.kwm.lastModifiedDate or "0"}";
    # The configuration of mzwm has a desktop color below the windows.
    withBackground = true;
  };
  kwim = pkgs.callPackage ./kwim.nix { src = inputs.kwim; };
  waylock = pkgs.callPackage "${inputs.waylock}/nix" {
    waylockSource = inputs.waylock;
    version = "0-unstable-${inputs.waylock.lastModifiedDate or "0"}";
  };

  # rofi with a patch: the list of a fullscreen window gets a new height when
  # the number of entries changes. The modal menus need this.
  rofi-unwrapped = pkgs.rofi-unwrapped.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [ ./rofi-relayout-list.patch ];
  });
}
