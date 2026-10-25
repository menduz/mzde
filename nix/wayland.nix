{
  pkgs,
  lib,
  config,
  ...
}:
# The Wayland session around river: the tools, the environment of the
# programs, the portals, polkit, and the screenshot commands.
let
  cfg = config.mzde;

  # `scrn`: a screenshot of all outputs. `slurpscrn`: a screenshot of an area
  # that the user selects. Each one writes ~/Screenshots/screenshot-<time>.png,
  # copies the image to the clipboard and shows a notification.
  screenshotScript =
    name: capture:
    pkgs.writeShellScriptBin name ''
      mkdir -p "$HOME/Screenshots"
      IMG_FILE="$HOME/Screenshots/screenshot-$(date +%Y-%m-%d-%H-%M-%S).png"
      ${capture} "$IMG_FILE"
      ${pkgs.wl-clipboard}/bin/wl-copy < "$IMG_FILE"
      ${pkgs.libnotify}/bin/notify-send -i "$IMG_FILE" -e -t 10000 "Screenshot Saved" "$IMG_FILE"
    '';
  scrn = screenshotScript "scrn" "${pkgs.grim}/bin/grim";
  slurpscrn = screenshotScript "slurpscrn" "${pkgs.slurp}/bin/slurp | ${pkgs.grim}/bin/grim -g -";

  # Turn the internal display of a laptop off and on.
  display = state: ''
    # Import the WAYLAND_DISPLAY env var from the compositor into the systemd user session.
    ${pkgs.dbus}/bin/dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP

    ${pkgs.wlr-randr}/bin/wlr-randr --output eDP-1 --${state}
  '';
  display-off = pkgs.writeShellScriptBin "display-off" (display "off");
  display-on = pkgs.writeShellScriptBin "display-on" (display "on");
in
{
  config = lib.mkIf cfg.enable {
    environment = {
      systemPackages = [
        pkgs.slurp
        pkgs.grim
        pkgs.wl-clipboard
        pkgs.libnotify
        pkgs.wdisplays
        pkgs.wlsunset
        pkgs.lswt
        pkgs.lshw-gui
        pkgs.wlr-randr
      ];
      sessionVariables = {
        WLR_NO_HARDWARE_CURSORS = "1";
        # Hint electron apps to use wayland
        NIXOS_OZONE_WL = "1";
        XDG_CURRENT_DESKTOP = "river";

        CLUTTER_BACKEND = "wayland";
        # SDL:
        SDL_VIDEODRIVER = "wayland";
        # QT (needs qt5.qtwayland in systemPackages):
        QT_QPA_PLATFORM = "wayland-egl";
        QT_WAYLAND_DISABLE_WINDOWDECORATION = "1";
        # Fix for some Java AWT applications (e.g. Android Studio),
        # use this if they aren't displayed properly:
        _JAVA_AWT_WM_NONREPARENTING = "1";
      };
    };

    xdg.portal = {
      wlr.enable = true;
      # Without a chooser, xdpw shares an arbitrary output.
      # The dmenu chooser shows the outputs and the windows in rofi.
      wlr.settings.screencast = {
        chooser_type = "dmenu";
        chooser_cmd = "${lib.getExe cfg.rofi.package} -dmenu -p Share";
      };
      # wlr.enable and the kwm-portal module add their own backend packages.
      extraPortals = [ pkgs.xdg-desktop-portal-gtk ];
      # https://bugs.debian.org/cgi-bin/bugreport.cgi?bug=1050913
      # https://github.com/emersion/xdg-desktop-portal-wlr/blob/master/contrib/wlroots-portals.conf
      # https://github.com/emersion/xdg-desktop-portal-wlr/pull/315
      config.river = {
        default = [
          "wlr"
          "gtk"
        ];
        # wlr handles capture; the kwm-portal module supplies the Inhibit route.
        "org.freedesktop.impl.portal.ScreenCast" = "wlr";
        "org.freedesktop.impl.portal.Screenshot" = "wlr";
      };
    };

    services.graphical-desktop.enable = true;

    # polkit-gnome needs polkit. The river module of NixOS also enables polkit,
    # but mzde does not use that module.
    security.polkit.enable = true;

    # Window manager only sessions (unlike DEs) don't handle XDG
    # autostart files, so force them to run the service
    services.xserver.desktopManager.runXdgAutostartIfNone = lib.mkDefault true;

    # Without other commands, the lid turns the internal display off and on.
    mzde.lidSwitch = {
      close = lib.mkDefault (lib.getExe display-off);
      open = lib.mkDefault (lib.getExe display-on);
    };

    mzde.bindings = {
      "Super+Alt 1" = {
        command = lib.getExe scrn;
        description = "Screenshot";
      };
      "Super+Alt 4" = {
        command = lib.getExe slurpscrn;
        description = "Screenshot of an area";
      };
    };

    home-manager.sharedModules = [
      {
        home.packages = [
          slurpscrn
          scrn

          display-off
          display-on

          pkgs.wlopm
        ];

        systemd.user.targets.graphical-session.Unit.Requires = [ cfg.sessionTarget ];

        # upgrade to enable GUI based pkexec
        services.polkit-gnome.enable = true;
        systemd.user.services.polkit-gnome.Unit.WantedBy = [ "xdg-desktop-autostart.target" ];
        systemd.user.services.polkit-gnome.Unit.Before = [ "xdg-desktop-autostart.target" ];
      }
    ];
  };
}
