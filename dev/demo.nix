# The demo system of the screenshots: mzde with the theme settings and the
# sandbox "code" of nix-config, the configuration of its author. The argument
# is the screenshot: "dark", "light", or "sandbox" (the dark scheme, with the
# sandbox "code").
scene:
{ config, ... }:
let
  variant = if scene == "sandbox" then "dark" else scene;
  theme = {
    inherit variant;
    name = "win-classic";
    # Redmond97 SE "Dusk Red".
    dark.preset = "dark";
    # The kwm bar of the dark scheme stays black, in the colors of mzde.
    dark.colors = with config.mzde.colors.bar; {
      barbg = background;
      barfg = foreground;
      barselectedfg = selected;
      baremptyfg = emptyTag;
    };
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

  mzde.enable = true;
  programs.win-classic-theme = theme;
  home-manager.sharedModules = [ { programs.win-classic-theme = theme; } ];

  programs.sandboxes = {
    enable = scene == "sandbox";
    environments.code = {
      description = "code and editors";
      winTheme.preset = "teal";
      backend = "bwrap";
      paths = [ "~/code" ];
      git = "read";
      ssh = "read";
    };
  };
  mzde.sandboxes.homeManagerUser = "demo";
  # The terminal of the sandbox floats above the terminals of the host, with
  # the title bar of kwm.
  mzde.floatRules = [
    {
      title = "~/code";
      decoration = "ssd";
    }
  ];
}
