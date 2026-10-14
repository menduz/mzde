{
  pkgs,
  lib,
  config,
  ...
}:
# toggle-theme: change the scheme of win-classic-theme at run time, without a
# rebuild. mzde puts it on Super+Shift O.
let
  cfg = config.mzde;

  # The theme that the theme module builds: one theme for each scheme, and
  # one theme with all schemes in one GTK4 style sheet.
  winTheme = config.programs.win-classic-theme.theme;

  # The schemes that toggle-theme selects. Without an argument, it changes
  # between light and dark. "contrast" is there only when the configuration
  # gives a contrast scheme.
  winSchemes = [
    "light"
    "dark"
  ]
  ++ lib.optional winTheme.package.hasContrast "contrast";

  # Change the scheme of win-classic-theme without a rebuild:
  #   - gtk-theme and icon-theme select the theme of the scheme by name. GTK3,
  #     GTK4, kwm and rofi follow it, also in a program that runs.
  #   - color-scheme and high-contrast go to the settings portal. A libadwaita
  #     program (Ghostty) has the theme with all schemes (GTK_THEME), and
  #     selects the scheme from these two values.
  # mzde puts this script on Super+Shift O.
  toggle-theme = pkgs.writeShellScriptBin "toggle-theme" ''
    set -euo pipefail

    uid=$(id -u)
    export DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$uid/bus"

    state="''${XDG_STATE_HOME:-$HOME/.local/state}/theme"
    schema="org.gnome.desktop.interface"
    gsettings="${pkgs.glib}/bin/gsettings"

    # kwm starts this script with the environment of the systemd user manager.
    # That environment has no directory with the GSettings schemas, and then
    # gsettings stops with "No schemas installed". Give the schemas and the
    # dconf backend here, thus the script does not depend on its environment.
    export XDG_DATA_DIRS="${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
    export GIO_EXTRA_MODULES="${pkgs.dconf.lib}/lib/gio/modules''${GIO_EXTRA_MODULES:+:$GIO_EXTRA_MODULES}"

    # The argument names the scheme. Without an argument, change between light
    # and dark. The scheme that is now active comes from the state file, or
    # from the theme that GSettings holds.
    if [ $# -gt 0 ]; then
      scheme="$1"
    else
      current=""
      if [ -r "$state/scheme" ]; then
        current=$(cat "$state/scheme")
      else
        case "$("$gsettings" get "$schema" gtk-theme)" in
          ${lib.concatMapStringsSep "\n      " (
            s: "\"'\"${winTheme.names.${s}}\"'\") current=${s} ;;"
          ) winSchemes}
        esac
      fi
      if [ "$current" = light ]; then scheme=dark; else scheme=light; fi
    fi

    case "$scheme" in
      ${lib.concatMapStringsSep "\n  " (
        s:
        "${s}) color_scheme=${
          if winTheme.package.dark.${s} then "prefer-dark" else "prefer-light"
        }; high_contrast=${lib.boolToString (s == "contrast")} ;;"
      ) winSchemes}
      *)
        echo "usage: toggle-theme [${lib.concatStringsSep "|" winSchemes}]" >&2
        exit 1
        ;;
    esac
    gtk_theme=${winTheme.name}-$scheme
    icon_theme=${
      if config.programs.win-classic-theme.iconTheme.name != null then
        lib.escapeShellArg config.programs.win-classic-theme.iconTheme.name
      else
        "$gtk_theme"
    }

    # Set the theme names first. GTK 4.22 has a fault: when GTK_THEME is set,
    # each change of gtk-theme loads the theme again and sets its color scheme
    # to light and its contrast to normal, but GtkSettings keeps the old
    # values. Thus a later change of gtk-theme puts libadwaita (Ghostty) in
    # the light scheme. Each change of the color scheme or of the contrast
    # after it sets the correct values again.
    "$gsettings" set "$schema" gtk-theme "$gtk_theme"
    "$gsettings" set "$schema" icon-theme "$icon_theme"
    "$gsettings" set "$schema" color-scheme "$color_scheme"
    "$gsettings" set org.gnome.desktop.a11y.interface high-contrast "$high_contrast"

    # Ghostty reads the color scheme and changes its palette without help.
    mkdir -p "$state"
    printf '%s\n' "$scheme" >"$state/scheme"
    ${pkgs.libnotify}/bin/notify-send -e -t 2000 -a toggle-theme \
      "Theme: $scheme" "$gtk_theme" || true
    printf '%s\n' "$scheme"
  '';
in
{
  options.mzde.toggleTheme = lib.mkOption {
    type = lib.types.package;
    readOnly = true;
    default = toggle-theme;
    description = ''
      `toggle-theme [light|dark|contrast]`: change the scheme of the theme
      without a rebuild. Without an argument, it changes between light and
      dark.
    '';
  };

  config = lib.mkIf cfg.enable {
    mzde.bindings."Super+Shift O" = {
      command = lib.getExe toggle-theme;
      description = "Light or dark theme";
    };

    home-manager.sharedModules = [ { home.packages = [ toggle-theme ]; } ];
  };
}
