{
  lib,
  pkgs,
  config,
  mzdeInputs,
  ...
}:
# rofi with the menu style of win-classic-theme. The theme of each scheme has
# its rofi theme in share/themes/<name>/rofi/win-classic.rasi. Refer to rofi/
# in win-classic-theme.
let
  winTheme = config.programs.win-classic-theme;
  cfg = config.mzde;

  # rofi with the patch of pkgs/default.nix, and the plugins of
  # mzde.rofi.plugins.
  rofiBase = pkgs.rofi.override {
    inherit
      (import ../pkgs {
        inherit pkgs;
        inputs = mzdeInputs;
      })
      rofi-unwrapped
      ;
  };
  rofi = rofiBase.override { inherit (cfg.rofi) plugins; };

  # The rofi theme of a theme below share/themes.
  rofiTheme = name: "${winTheme.theme.package}/share/themes/${name}/rofi/win-classic.rasi";

  # The rofi theme of a theme as a modal dialog. A fullscreen window darkens
  # the output, and the menu frame is in a box at the center. The frame boxes
  # are the same as the frame of window, mainbox and bevel in
  # rofi/win-classic.rasi of win-classic-theme.
  modalTheme =
    name:
    pkgs.writeText "${name}-modal.rasi" ''
      @import "${rofiTheme name}"

      window {
          fullscreen:       true;
          background-color: rgba(0, 0, 0, 50%);
          border:           0px;
          padding:          0px;
      }

      mainbox {
          border:           0px;
          children:         [ modal-above, dialog, modal-below ];
      }

      modal-above, modal-below {
          expand:           true;
      }

      dialog {
          expand:           false;
          margin:           0px calc((100% - 30em) / 2);
          background-color: @face;
          border:           0px 1px 1px 0px;
          border-color:     @frame;
          padding:          1px 0px 0px 1px;
          children:         [ dialog-edge ];
      }

      dialog-edge {
          border:           1px 0px 0px 1px;
          border-color:     @light;
          children:         [ bevel ];
      }
    '';

  # toggle-theme changes the GTK theme at run time. This command reads the
  # GTK theme from GSettings each time, and gives the rofi theme of that
  # scheme. Without a match, it uses the scheme of the build.
  #
  # The menu is a modal dialog. With --no-modal as the first argument, the
  # menu is a window without the dark layer (for menus at a position).
  mz-rofi = pkgs.writeShellScriptBin "mz-rofi" ''
    modal=1
    if [ "''${1-}" = --no-modal ]; then
      modal=
      shift
    fi
    gtk_theme=$(GIO_EXTRA_MODULES=${pkgs.dconf.lib}/lib/gio/modules \
      ${pkgs.glib}/bin/gsettings get org.gnome.desktop.interface gtk-theme 2>/dev/null | tr -d "'")
    case "$gtk_theme" in
      ${lib.concatMapStringsSep "\n  " (
        name:
        "${lib.escapeShellArg name}) theme=${lib.escapeShellArg (rofiTheme name)} modal_theme=${modalTheme name} ;;"
      ) (lib.unique (lib.attrValues winTheme.theme.names))}
      *) theme=${lib.escapeShellArg (rofiTheme winTheme.theme.variantName)} modal_theme=${modalTheme winTheme.theme.variantName} ;;
    esac
    if [ -n "$modal" ]; then
      theme=$modal_theme
    fi
    exec ${rofi}/bin/rofi -theme "$theme" "$@"
  '';
in
{
  config = lib.mkMerge [
    { mzde.rofi.package = mz-rofi; }
    (lib.mkIf cfg.enable {
      home-manager.sharedModules = [
        {
          home.packages = [ mz-rofi ];

          programs.rofi = {
            enable = true;
            package = rofi;
            # A plain `rofi` uses the scheme of the build, as a modal dialog.
            theme = "${modalTheme winTheme.theme.variantName}";
            # The interface font of the theme, as GTK uses it.
            font = "${winTheme.fontName} ${toString winTheme.baseFontSize}";
          };
        }
      ];
    })
  ];
}
