# The demo system of the screenshots: mzwm with the theme settings of
# nix-config, the configuration of its author. The argument is the variant of
# the theme: "dark" or "light".
variant:
{ ... }:
let
  theme = {
    inherit variant;
    name = "win-classic";
    # Redmond97 SE "Dusk Red".
    dark.preset = "dark";
    # The Windows XP classic scheme "Windows Standard".
    light.preset = "windows-standard";
    # kwm tiles the windows, so no window has these buttons.
    titlebarButtons = {
      minimize = false;
      maximize = false;
    };
    decorationLayout = ":menu";
  };
in
{
  boot.loader.grub.device = "nodev";
  fileSystems."/" = {
    device = "/dev/sda1";
    fsType = "ext4";
  };
  system.stateVersion = "26.05";

  users.users.demo.isNormalUser = true;
  home-manager.useGlobalPkgs = true;
  home-manager.users.demo.home.stateVersion = "26.05";

  # The theme draws with the first sans serif font.
  fonts.fontconfig.defaultFonts.sansSerif = [ "Inter" ];

  mzwm.enable = true;
  programs.win-classic-theme = theme;
  home-manager.sharedModules = [ { programs.win-classic-theme = theme; } ];
}
