{
  lib,
  pkgs,
  ...
}:
# The options of mzde. Other modules add bindings, float rules and startup
# commands through these options. They do not write the configuration of kwm
# directly.
let
  zon = import ./zon.nix { inherit lib; };

  binding = lib.types.submodule {
    options = {
      command = lib.mkOption {
        type = lib.types.str;
        description = "The shell command that the binding runs.";
      };
      whenLocked = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Also run the command while the session is locked.";
      };
      description = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          The text of the binding in the overlay of the Super key (`kwm
          bindings`). null: kwm shows the name of the program.
        '';
      };
    };
  };

  windowRule = lib.types.submodule {
    options = {
      appId = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "The app-id of the window. null matches all app-ids.";
      };
      title = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "The title of the window. null matches all titles.";
      };
    };
  };

  widgets =
    where:
    lib.mkOption {
      type = lib.types.listOf lib.types.attrs;
      default = [ ];
      description = ''
        The widgets of kwm in the ${where} of the bar, as Nix values for
        config.zon. Refer to src/config/widget.zig in kwm. mzde adds its own
        widgets with lib.mkOrder. A value without an order goes between them.
        Use mzde.lib to make a widget.
      '';
    };
in
{
  options.mzde = {
    enable = lib.mkEnableOption "mzde, the river desktop with kwm";

    sessionTarget = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      default = "river-session.target";
      description = ''
        The systemd user target of the session. It starts after the
        compositor sets WAYLAND_DISPLAY. Bind the session services to it.
      '';
    };

    sessionHook = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = ''
        The script that starts mzde.sessionTarget. The window manager runs it
        once, after the compositor sets WAYLAND_DISPLAY.
      '';
    };

    lib = lib.mkOption {
      type = lib.types.attrs;
      readOnly = true;
      default = {
        inherit zon;
        # A script widget of kwm. The script writes JSON lines: text, tooltip
        # and class. interval = 0: the script writes a line for each change.
        scriptWidget =
          {
            exec,
            interval ? 0,
            onClick ? null,
          }:
          {
            script = {
              inherit exec interval;
              return_type = zon.enum "json";
            }
            // lib.optionalAttrs (onClick != null) { on_click = onClick; };
          };
      };
      description = "Helpers to make the values of mzde.bar and mzde.kwm.settings.";
    };

    bindings = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.coercedTo lib.types.str (command: { inherit command; }) binding
      );
      default = { };
      example = {
        "Super Return" = "ghostty";
        "None XF86AudioMute" = {
          command = "wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle";
          whenLocked = true;
        };
      };
      description = ''
        Key bindings that run a shell command. The name has the modifiers, a
        space, and the xkb keysym. Join the modifiers with "+". Use "None" for
        no modifier.

        The modifiers are Super, Alt, Control, Shift, Mod3 and Mod5.
      '';
    };

    floatRules = lib.mkOption {
      type = lib.types.listOf windowRule;
      default = [ ];
      description = "The windows that float. A rule matches the full app-id and the full title.";
    };

    startupCommands = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Shell commands that the window manager starts in the background.";
    };

    terminal = lib.mkOption {
      type = lib.types.str;
      default = "${pkgs.ghostty}/bin/ghostty";
      defaultText = lib.literalExpression ''"''${pkgs.ghostty}/bin/ghostty"'';
      description = ''
        The terminal. The bar starts programs in it with `<terminal> -e
        <program>`.
      '';
    };

    keymap = {
      layout = lib.mkOption {
        type = lib.types.str;
        default = "us";
        description = "The XKB layout of all keyboards.";
      };
      variant = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "The XKB variant of all keyboards.";
      };
      options = lib.mkOption {
        type = lib.types.str;
        default = "";
        example = "caps:escape";
        description = "The XKB options of all keyboards.";
      };
    };

    keyRepeat = {
      rate = lib.mkOption {
        type = lib.types.ints.positive;
        default = 25;
        description = "The key repeats in each second.";
      };
      delay = lib.mkOption {
        type = lib.types.ints.positive;
        default = 600;
        description = "The time in milliseconds before a held key repeats.";
      };
    };

    naturalScroll = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Natural scrolling on touchpads and mice.";
    };

    colors = lib.mkOption {
      type = lib.types.attrs;
      default = import ./colors.nix;
      defaultText = lib.literalExpression "import mzde/nix/colors.nix";
      description = ''
        The colors of the bar, the borders and the lock screen. Refer to
        nix/colors.nix for the names. The win-classic theme gives the colors
        of the title bars and the 3D borders.
      '';
    };

    bar = {
      center = widgets "center";
      right = widgets "right part";
      end = widgets "right end, after the tray";

      temperature = {
        ignore = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [ ];
          example = [ "legion_hwmon" ];
          description = ''
            The hwmon devices (the file `name` in /sys/class/hwmon/hwmon*)
            that the temperature meter ignores, for example a sensor that
            gives wrong values.
          '';
        };
        nvidiaSmi = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = ''
            The temperature meter also shows the NVIDIA GPUs with nvidia-smi.
            The NVIDIA driver has no hwmon device. kwm does not start
            nvidia-smi while the GPUs sleep (runtime D3).
          '';
        };
      };
    };

    kwm.settings = lib.mkOption {
      type = lib.types.attrs;
      default = { };
      description = ''
        Values for config.zon of kwm. They replace the values of mzde
        (lib.recursiveUpdate). Use mzde.lib.zon for enums.
      '';
    };

    lock = {
      afterUnlock = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = ''
          A shell command that runs after the unlock, for example to apply the
          output profile again.
        '';
      };
      lockAfter = lib.mkOption {
        type = lib.types.ints.positive;
        default = 300;
        description = "The idle time in seconds before the screen locks.";
      };
      screenOffAfter = lib.mkOption {
        type = lib.types.ints.positive;
        default = 400;
        description = "The idle time in seconds before the outputs turn off.";
      };
      sleepAfter = lib.mkOption {
        type = lib.types.nullOr lib.types.ints.positive;
        default = 1000;
        description = "The idle time in seconds before the system sleeps. null: never.";
      };
    };

    rofi = {
      package = lib.mkOption {
        type = lib.types.package;
        readOnly = true;
        description = ''
          `mz-rofi`: rofi with the win-classic menu style of the current
          scheme, as a modal dialog. Use it for each rofi command.
          `mz-rofi --no-modal ...` shows a menu without the dark layer.
        '';
      };
      plugins = lib.mkOption {
        type = lib.types.listOf lib.types.package;
        default = [ ];
        example = lib.literalExpression "[ pkgs.rofi-calc ]";
        description = "The plugins of rofi.";
      };
    };
  };
}
