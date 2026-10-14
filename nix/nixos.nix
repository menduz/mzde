# The NixOS module of mzde. It also gives the Home Manager part of the desktop
# to each Home Manager user (home-manager.sharedModules). Thus the system
# needs the NixOS module of Home Manager.
#
# The argument is the inputs of flake.nix. The imports need them, thus they
# cannot come from a module argument. The other files get them as the module
# argument mzdeInputs.
mzdeInputs:
{
  config,
  lib,
  ...
}:
let
  cfg = config.mzde;
  winClassicTheme = mzdeInputs.win-classic-theme;

  # The defaults of mzde for the theme. NixOS and Home Manager each have the
  # options of the theme, so both get these values.
  themeDefaults = {
    enabled = lib.mkDefault true;
    # The size of the interface font, in points. GTK calculates the pixels
    # from the DPI: pixels = points x 96 / 72. A size that divides by 3 gives
    # a whole number of pixels: 9 points give 12 pixels. The default of the
    # theme, 8 points, gives 10.67 pixels, and the text is then not sharp.
    baseFontSize = lib.mkDefault 9;
    fontRendering.dpi = lib.mkDefault 96;
  };
in
{
  imports = [
    { _module.args.mzdeInputs = mzdeInputs; }
    ./options.nix
    ./session.nix
    ./river.nix
    ./lock.nix
    ./rofi.nix
    ./theme.nix
    ./wayland.nix
    ./widgets.nix
    ../kwm-portal/nix/modules/nixos.nix
    "${winClassicTheme}/nixos-module.nix"
  ];

  config = lib.mkIf cfg.enable {
    # The theme of GTK, kwm and rofi. Set the schemes with
    # programs.win-classic-theme, in NixOS and in Home Manager.
    programs.win-classic-theme = themeDefaults;
    home-manager.sharedModules = [
      "${winClassicTheme}/home-manager-module.nix"
      { programs.win-classic-theme = themeDefaults; }
    ];

    # kwm-portal gives the Inhibit portal: a program can stop the idle timers
    # of the session.
    services.kwm-portal.enable = lib.mkDefault true;
  };
}
