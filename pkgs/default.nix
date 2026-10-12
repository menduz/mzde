# The packages of mzwm, built with the given nixpkgs from the sources in the
# inputs of flake.nix.
{ pkgs, inputs }:
{
  river = pkgs.callPackage ./river.nix { src = inputs.river; };
  kwm = pkgs.callPackage ./kwm.nix { src = inputs.kwm; };
  kwim = pkgs.callPackage ./kwim.nix { src = inputs.kwim; };
  waylock = pkgs.callPackage ./waylock.nix { src = inputs.waylock; };

  # rofi with a patch: the list of a fullscreen window gets a new height when
  # the number of entries changes. The modal menus need this.
  rofi-unwrapped = pkgs.rofi-unwrapped.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [ ./rofi-relayout-list.patch ];
  });
}
