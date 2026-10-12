# The NixOS module of mzwm. It also gives the Home Manager part of the desktop
# to each Home Manager user (home-manager.sharedModules). Thus the system
# needs the NixOS module of Home Manager.
#
# The argument is the inputs of flake.nix. The imports need them, thus they
# cannot come from a module argument. The other files get them as the module
# argument mzwmInputs.
mzwmInputs:
{
  config,
  lib,
  ...
}:
let
  cfg = config.mzwm;
  winClassicTheme = mzwmInputs.win-classic-theme;
in
{
  imports = [
    { _module.args.mzwmInputs = mzwmInputs; }
    ./options.nix
    ./session.nix
    ./river.nix
    ./lock.nix
    ./rofi.nix
    ../kwm-portal/nix/modules/nixos.nix
    "${winClassicTheme}/nixos-module.nix"
  ];

  config = lib.mkIf cfg.enable {
    home-manager.sharedModules = [ "${winClassicTheme}/home-manager-module.nix" ];

    # kwm-portal gives the Inhibit portal: a program can stop the idle timers
    # of the session.
    services.kwm-portal.enable = lib.mkDefault true;
  };
}
