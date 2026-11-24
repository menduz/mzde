{
  config,
  lib,
  pkgs,
  mzdeInputs,
  ...
}:
# The sandboxes in the desktop. programs.sandboxes (../sandboxes) gives the
# sandboxes and the `sandbox` command, as the qubes of Qubes OS. With
# programs.sandboxes.enable, mzde gives:
#
#   - a win-classic theme for each sandbox (GTK and the rofi menu): the
#     preset in winTheme.preset of the sandbox, and a dark scheme in the
#     colors of the sandbox. The sandbox follows the color scheme of the
#     host (followColorScheme), so toggle-theme changes the sandboxes too,
#   - the Ghostty configuration of each sandbox: mzde.sandboxes.ghostty.config
#     with a background and a cursor in the sandbox color,
#   - the read-only dotfiles of Home Manager in each sandbox
#     (mzde.sandboxes.homeManagerUser),
#   - the menus: Super+Space and Super+Return ask for the sandbox first, and
#     Super+Shift+Return starts Ghostty on the host. Super+Space shows only
#     the sandboxes with gui.
#
# kwm finds the sandbox of each window from the cgroup of its process
# (sandbox-<name>.slice), and shows the name on the window and in the bar
# (sandbox.label in river.nix).
let
  cfg = config.mzde.sandboxes;
  sandboxes = config.programs.sandboxes;
  envs = sandboxes.environments;
  enabled = config.mzde.enable && sandboxes.enable;

  winTheme = config.programs.win-classic-theme;
  presets = import "${mzdeInputs.win-classic-theme}/presets.nix";
  termColors = import "${mzdeInputs.mz-term-colors}/lib" { inherit pkgs; };

  sandbox = lib.getExe sandboxes.package;
  ghostty = lib.getExe cfg.ghostty.package;

  hm = if cfg.homeManagerUser == null then null else config.home-manager.users.${cfg.homeManagerUser};
  hmFiles = if hm == null then [ ] else lib.attrValues hm.home.file;
  hmFile = target: lib.findFirst (f: lib.removePrefix "./" f.target == target) null hmFiles;

  # The files of Home Manager that the sandboxes do not get: the files of
  # the session and of the desktop. The sandbox gets its own Ghostty
  # configuration, and the options ssh, gpg and git of the sandbox give the
  # rest.
  hiddenHomeFiles = [
    ".config/systemd/"
    ".config/environment.d/"
    ".local/share/dbus-1/"
    ".config/kwm/"
    ".config/kwim/"
    ".config/river/"
    ".config/kanshi/"
    ".config/dunst/"
    ".config/ghostty/"
    ".config/git/"
    ".gnupg/"
    ".background-image"
  ]
  ++ cfg.hiddenHomeFiles;

  # The files of Home Manager that each sandbox gets, read-only.
  dotfiles = lib.filter (
    path:
    !(lib.any (prefix: lib.hasPrefix prefix path) hiddenHomeFiles) && !(lib.hasSuffix "/.keep" path)
  ) (map (f: lib.removePrefix "./" f.target) hmFiles);

  rofiTheme = e: "${e.winTheme.package}/share/themes/${e.gtkTheme}/rofi/win-classic.rasi";

  guiEnvs = lib.filterAttrs (_: e: e.gui) envs;

  # Super+Space: select the sandbox, and then the program. A sandbox without
  # gui cannot show a window, so the menu does not show it.
  drun = pkgs.writeShellScriptBin "mzde-sandbox-drun" ''
    name=$(${sandbox} pick --host --gui) || exit 0
    case "$name" in
      # `sandbox host` starts each program in its own scope below app.slice,
      # not in the cgroup of the window manager.
      host) exec ${lib.getExe config.mzde.rofi.package} -combi-modi drun -show combi -show-icons \
        -run-command "${sandbox} host -- {cmd}" \
        -run-shell-command "${sandbox} host -- {terminal} -e {cmd}" ;;
      ${lib.concatStringsSep "\n  " (
        lib.mapAttrsToList (
          n: e: "${n}) theme=${config.mzde.rofi.modalTheme "sandbox-${n}" (rofiTheme e)} ;;"
        ) guiEnvs
      )}
      *) exit 1 ;;
    esac
    # rofi runs the Exec line of the desktop file with -run-command. A program
    # that D-Bus starts is outside the sandbox, so D-Bus activation is off.
    exec ${lib.getExe config.mzde.rofi.unwrapped} -theme "$theme" -show drun -show-icons -display-drun "$name" \
      -theme-str 'configuration { drun { DBusActivatable: false; } }' \
      -run-command "${sandbox} run $name -- {cmd}" \
      -run-shell-command "${sandbox} run $name -- {terminal} -e {cmd}"
  '';

  # Super+Shift+Return: Ghostty on the host, with the label of the host
  # (SANDBOX_NAME=host). A new process is necessary: a window of a Ghostty
  # that runs already has the environment of that process.
  hostTerminal = pkgs.writeShellScriptBin "mzde-host-terminal" ''
    exec ${sandbox} host -- ${ghostty} --gtk-single-instance=false
  '';

  # The terminal of a sandbox. A sandbox with gui gets Ghostty in the sandbox.
  # In a sandbox without gui (gVisor: no Wayland client can draw in it),
  # Ghostty runs on the host and only its shell is in the sandbox. That
  # Ghostty runs in the slice of the sandbox, so kwm shows the label of the
  # sandbox on the window. It has the Ghostty configuration and the GTK theme
  # of the sandbox.
  terminalOf =
    n: e:
    if e.gui then
      "exec ${sandbox} run ${n} -- ${ghostty}"
    else
      lib.concatStringsSep " " [
        "exec ${pkgs.systemd}/bin/systemd-run --user --scope --quiet --collect"
        "--slice=sandbox-${n}.slice --unit=sandbox-${n}-terminal-$$-$RANDOM"
        "--description=${lib.escapeShellArg "sandbox ${n}: terminal"}"
        "-- env GTK_THEME=${e.gtkTheme}"
        "${ghostty} --config-default-files=false"
        "--config-file=${e.files.".config/ghostty/config".source}"
        "-e ${sandbox} run ${n}"
      ];

  # Super+Return: select the sandbox, and then start Ghostty for it.
  terminal = pkgs.writeShellScriptBin "mzde-sandbox-terminal" ''
    name=$(${sandbox} pick --host) || exit 0
    case "$name" in
      host) exec ${lib.getExe hostTerminal} ;;
      ${lib.concatStringsSep "\n  " (lib.mapAttrsToList (n: e: "${n}) ${terminalOf n e} ;;") envs)}
      *) exit 1 ;;
    esac
  '';

  # The colors of the dark scheme of a sandbox that the light scheme gives:
  # the title bar and the selection have the color of the sandbox also in
  # the dark scheme.
  sandboxColorKeys = [
    "activetitle"
    "activetitle1"
    "activetitletext"
    "selectedbg"
    "selectedtext"
  ];

  # The options that mzde adds to each sandbox. The options are always
  # there, so a configuration can set them also without mzde.enable.
  sandboxOptions =
    { name, config, ... }:
    let
      t = config.winTheme;
      lightColors = presets.${t.preset} // t.colors;
    in
    {
      options.winTheme = {
        preset = lib.mkOption {
          type = lib.types.enum (lib.attrNames presets);
          default = "windows-standard";
          description = "The win-classic preset of the light scheme of the sandbox.";
        };
        colors = lib.mkOption {
          type = lib.types.attrsOf lib.types.str;
          default = { };
          description = "Colors that replace colors of the preset.";
        };
        darkPreset = lib.mkOption {
          type = lib.types.enum (lib.attrNames presets);
          # A preset can have a dark scheme of its own: "teal-dark" for
          # "teal".
          default = if presets ? "${t.preset}-dark" then "${t.preset}-dark" else "dark";
          defaultText = lib.literalMD ''"<preset>-dark" if that preset exists, else "dark"'';
          description = "The win-classic preset of the dark scheme of the sandbox.";
        };
        darkColors = lib.mkOption {
          type = lib.types.attrsOf lib.types.str;
          default = lib.getAttrs (lib.filter (k: lightColors ? ${k}) sandboxColorKeys) lightColors;
          defaultText = lib.literalMD "the title bar and selection colors of the light scheme";
          description = "Colors that replace colors of `darkPreset`.";
        };
        dark = lib.mkOption {
          type = lib.types.bool;
          default = t.preset == "dark";
          defaultText = lib.literalExpression ''winTheme.preset == "dark"'';
          description = ''
            Whether the light scheme is a dark one (color-scheme
            prefer-dark). Such a sandbox does not follow the color scheme of
            the host (followColorScheme is false).
          '';
        };
        package = lib.mkOption {
          type = lib.types.package;
          readOnly = true;
          # The settings of the schemes of the host: the title bar buttons and
          # the style sheet for kwm.
          default = winTheme.mkPackage {
            name = "win-classic-sandbox-${name}";
            colorSchemes = {
              light = { inherit (t) preset colors; };
              dark = {
                preset = t.darkPreset;
                colors = t.darkColors;
              };
            };
          };
          defaultText = lib.literalMD "the theme of the presets and the colors";
          description = "The built theme.";
        };
      };
    };

  # The settings that mzde gives to each sandbox.
  sandboxDefaults =
    { name, config, ... }:
    let
      themeName = "win-classic-sandbox-${name}";
      # The GTK settings of the host (font, DPI, font rendering, toolbars)
      # with the theme and the icons of the sandbox.
      gtkSettings = winTheme.settingsFor {
        name = themeName;
        inherit (config.winTheme) dark;
      };
      toIni = settings: lib.generators.toINI { } { Settings = settings; };
      hostDconf = if hm == null then { } else hm.dconf.settings;
      gtkrc = hmFile ".gtkrc-2.0";
    in
    {
      gtkTheme = lib.mkDefault themeName;

      # gtkTheme is the theme with all schemes: GTK4 and libadwaita select the
      # dark scheme of the sandbox from the color scheme, which follows the
      # host. A sandbox with a dark light scheme stays dark.
      followColorScheme = lib.mkDefault (!config.winTheme.dark);
      # kwm draws the title bars of the sandbox with this theme when the host
      # prefers dark, as the programs in the sandbox select the dark scheme.
      gtkThemeDark = lib.mkDefault (
        if config.followColorScheme then config.winTheme.package.names.dark else null
      );

      # GTK on Wayland reads the fonts, the cursor and the icon theme from
      # GSettings. These are the values of the host, with the scheme of the
      # sandbox.
      dconfSettings = hostDconf // {
        "org/gnome/desktop/interface" = (hostDconf."org/gnome/desktop/interface" or { }) // {
          gtk-theme = themeName;
          icon-theme = themeName;
          color-scheme = if config.winTheme.dark then "prefer-dark" else "prefer-light";
        };
      };

      # The settings files of Home Manager name the theme of the host. Each
      # sandbox gets the same files with its own scheme.
      files = {
        ".config/gtk-3.0/settings.ini".text = toIni gtkSettings.gtk3;
        ".config/gtk-4.0/settings.ini".text = toIni (
          gtkSettings.gtk4
          // {
            # 2 is dark and 3 is light (GtkInterfaceColorScheme).
            gtk-interface-color-scheme = if config.winTheme.dark then 2 else 3;
          }
        );
        # GTK4 reads no theme by name. The style sheet of the user imports the
        # theme, as Home Manager does for the host.
        ".config/gtk-4.0/gtk.css".text = ''
          @import url("file://${config.winTheme.package}/share/themes/${themeName}/gtk-4.0/gtk.css");
          ${winTheme.css}
        '';

        ".config/ghostty/config".text =
          let
            # The dark theme of the sandbox color: the dark background of the
            # host with a tint of the color.
            theme = termColors.ghosttyTheme { inherit (config) color; };
          in
          cfg.ghostty.config
          + ''

            # The sandbox ${name}. A key here replaces the same key above.
            # The theme is always the dark theme. The color scheme in the
            # sandbox is the scheme of the sandbox, and most of these schemes
            # are light.
            theme = ${theme}
            # Each run starts its own Ghostty. An instance that runs already
            # must not open the window.
            gtk-single-instance = false
          '';
      }
      # GTK2 and the gtk2 style of Qt read the theme from this file.
      // lib.optionalAttrs (gtkrc != null) {
        ".gtkrc-2.0".text =
          builtins.replaceStrings
            [ "\"${winTheme.theme.variantName}\"" "\"${winTheme.theme.iconTheme}\"" ]
            [ "\"${themeName}\"" "\"${themeName}\"" ]
            gtkrc.text;
      };

      # The title bar color of the preset.
      color = lib.mkDefault (
        config.winTheme.colors.activetitle or presets.${config.winTheme.preset}.activetitle
      );

      # The variables of Home Manager (toolkit settings, XDG directories).
      passEnv = lib.optionals (hm != null) (lib.attrNames hm.home.sessionVariables);

      homePatterns = dotfiles;
    };
in
{
  options.mzde.sandboxes = {
    homeManagerUser = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "alice";
      description = ''
        The user whose Home Manager files each sandbox gets, read-only: the
        dotfiles, the GSettings values (dconf.settings), .gtkrc-2.0 and the
        session variables. null: no files of Home Manager.
      '';
    };

    hiddenHomeFiles = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ ".config/keepassxc/" ];
      description = ''
        More prefixes of the Home Manager files that the sandboxes do not
        get. mzde already hides the files of the session and of the desktop
        (systemd, D-Bus, river, kwm, kanshi, dunst, Ghostty, git, GnuPG).
      '';
    };

    ghostty = {
      package = lib.mkOption {
        type = lib.types.package;
        default = pkgs.ghostty;
        defaultText = lib.literalExpression "pkgs.ghostty";
        description = "Ghostty, the terminal of the sandboxes and of the host.";
      };
      config = lib.mkOption {
        type = lib.types.lines;
        default = "";
        description = ''
          The Ghostty configuration of the sandboxes. Each sandbox adds a
          theme in its color.
        '';
      };
    };

    drun = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = drun;
      defaultText = lib.literalMD "the menu";
      description = "The program menu: select the sandbox, and then the program.";
    };
    terminal = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = terminal;
      defaultText = lib.literalMD "the menu";
      description = "The terminal menu: select the sandbox, and then start Ghostty in it.";
    };
    hostTerminal = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      default = hostTerminal;
      defaultText = lib.literalMD "the command";
      description = "Ghostty on the host, with the label of the host.";
    };
  };

  config = lib.mkMerge [
    { programs.sandboxes.defaults = sandboxOptions; }
    (lib.mkIf enabled {
      programs.sandboxes = {
        defaults = sandboxDefaults;

        # The label of the host: the gray of the 16 colors of Windows, as the
        # colors of the presets.
        host.color = lib.mkDefault "#808080";

        menu = {
          command = lib.mkDefault "${lib.getExe config.mzde.rofi.package} -dmenu -i -no-custom -markup-rows -p sandbox";
          markup = lib.mkDefault true;
        };
      };

      mzde.bindings = {
        "Super Space" = lib.mkDefault {
          command = lib.getExe drun;
          description = "Start a program";
        };
        "Super Return" = lib.mkDefault {
          command = lib.getExe terminal;
          description = "Terminal in a sandbox";
        };
        #"Super+Shift Return" = lib.mkDefault {
        #  command = lib.getExe hostTerminal;
        #  description = "Terminal on the host";
        #};
      };

      # GTK finds a theme only in XDG_DATA_DIRS.
      environment.systemPackages = lib.mapAttrsToList (_: e: e.winTheme.package) envs ++ [
        drun
        terminal
        hostTerminal
      ];
    })
  ];
}
