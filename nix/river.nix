{
  pkgs,
  lib,
  config,
  mzdeInputs,
  ...
}:
# river5: river from the main branch (0.6.0-dev, after the release of 0.5.0) with the kwm window manager.
#
# This river is non-monolithic. The compositor has no window manager, no
# riverctl and no rivertile. A separate window manager process gives the
# policy through river-window-management-v1. This module uses kwm. kwm starts
# kwim, and kwim configures the input devices. Refer to nix/default.nix in
# the river and kwm repositories, and to pkgs/kwim.nix.
#
# This module configures only the window manager. It reads the mzde options and
# writes $XDG_CONFIG_HOME/river/init, $XDG_CONFIG_HOME/kwm/config.zon and
# $XDG_CONFIG_HOME/kwim/config.zon, and the user service kwm.service.
# session.nix gives the session target and the media keys.
#
# Notes:
#   - river 0.5 does not send lid switch events to the window manager.
#   - river 0.5 does not have river-status-unstable-v1, thus there is no waybar.
#     The bar of kwm at the top shows the tags, the layout, the window title,
#     the widgets of the kwm fork and the tray.
#   - kwm draws the background in the color of the desktop (withBackground in
#     pkgs/default.nix).
#   - kwm moves a window to an other output without the tags of that output.
let
  cfg = config.mzde;
  inherit (cfg) colors;
  zon = import ./zon.nix { inherit lib; };
  inherit (zon) rgba;

  # river keeps Xwayland. Qt programs need an X display for the gtk2 platform
  # theme of Qt.
  packages = import ../pkgs {
    inherit pkgs;
    inputs = mzdeInputs;
  };
  inherit (packages) river kwm kwim;

  # "Super+Shift" gives the modifiers struct of kwm.
  modifiers =
    text:
    let
      names = {
        Super = "mod4";
        Alt = "mod1";
        Control = "ctrl";
        Shift = "shift";
        Mod3 = "mod3";
        Mod5 = "mod5";
      };
      toField =
        name:
        if names ? ${name} then
          lib.nameValuePair names.${name} true
        else
          throw "river5: unknown modifier `${name}`. Refer to the description of mzde.bindings.";
    in
    lib.listToAttrs (map toField (lib.remove "None" (lib.splitString "+" text)));

  # "Super+Shift Return", a description and an action give a key binding of
  # kwm. The overlay of the Super key shows the description (`kwm bindings`).
  # null: kwm shows a text from the action.
  key =
    mode: chord: description: action:
    let
      parts = lib.splitString " " chord;
    in
    if lib.length parts != 2 then
      throw "river5: the binding `${chord}` must have the format `<modifiers> <keysym>`."
    else
      {
        inherit mode description;
        keysym = lib.elemAt parts 1;
        modifiers = modifiers (lib.elemAt parts 0);
        event.click.pressed = action;
      };

  spawn = command: { spawn_shell.cmd = command; };

  forward.direction = zon.enum "forward";
  reverse.direction = zon.enum "reverse";
  allTags = 511; # 0b111111111, the 9 tags.

  # A binding of the window manager: the description and the action.
  wm = description: action: { inherit description action; };

  windowManagerBindings = {
    "Super Q" = wm "Close the window" (zon.enum "close");
    "Super+Shift Space" = wm "Float or tile the window" (zon.enum "toggle_floating");
    "Super+Shift F" = wm "Fullscreen" {
      toggle_fullscreen.in_window = false;
    };

    "Alt Return" = wm "Swap the window with the master" {
      zoom.swap = true;
    };
    "Super J" = wm "Focus the previous window" {
      focus_iter = reverse;
    };
    "Super K" = wm "Focus the next window" {
      focus_iter = forward;
    };

    "Super H" = wm "Smaller master area" {
      modify_mfact.change.step = -0.05;
    };
    "Super L" = wm "Larger master area" {
      modify_mfact.change.step = 0.05;
    };

    "Super+Shift H" = wm "More master windows" {
      modify_nmaster.change = zon.enum "increase";
    };
    "Super+Shift L" = wm "Less master windows" {
      modify_nmaster.change = zon.enum "decrease";
    };

    "Super Up" = wm "Focus the next window" {
      focus_iter = forward;
    };
    "Super Down" = wm "Focus the previous window" {
      focus_iter = reverse;
    };
    "Super+Shift J" = wm "Move the window down the stack" {
      swap = forward;
    };
    "Super+Shift K" = wm "Move the window up the stack" {
      swap = reverse;
    };
    "Super+Shift Up" = wm "Move the window down the stack" {
      swap = forward;
    };
    "Super+Shift Down" = wm "Move the window up the stack" {
      swap = reverse;
    };
    "Super Period" = wm "Focus the next output" {
      focus_output_iter = forward;
    };
    "Super Comma" = wm "Focus the previous output" {
      focus_output_iter = reverse;
    };

    "Super O" = wm "Move the window to the next output, and follow it" {
      group.actions = [
        { send_to_output = forward; }
        { focus_output_iter = forward; }
      ];
    };

    # change monitor
    "Super+Shift Period" = wm "Move the window to the next output" {
      send_to_output = forward;
    };
    "Super+Shift Comma" = wm "Move the window to the previous output" {
      send_to_output = reverse;
    };

    # The bindings of a digit have one description for all digits. The
    # overlay then shows them in one line: "Super+0..9".
    "Super 0" = wm "Switch workspace (0: all)" {
      set_output_tag.tag.tag = allTags;
    };
    "Super+Shift 0" = wm "Move the window to a workspace (0: all)" {
      set_window_tag.tag.tag = allTags;
    };
  }
  // lib.mergeAttrsList (
    map (
      n:
      let
        # 2^(n-1): the bit of tag n.
        tag = lib.foldl (acc: _: acc * 2) 1 (lib.range 2 n);
        x = toString n;
      in
      {
        "Super ${x}" = wm "Switch workspace (0: all)" {
          set_output_tag.tag.tag = tag;
        };
        "Super+Shift ${x}" = wm "Move the window to a workspace (0: all)" {
          set_window_tag.tag.tag = tag;
        };
        "Super+Control ${x}" = wm "Also show a workspace" {
          toggle_output_tag.mask = tag;
        };
        "Super+Shift+Control ${x}" = wm "Also put the window on a workspace" {
          toggle_window_tag.mask = tag;
        };
      }
    ) (lib.range 1 9)
  );

  keyBindings =
    lib.mapAttrsToList (chord: b: key "default" chord b.description b.action) windowManagerBindings
    ++ lib.mapAttrsToList (
      chord: binding: key "default" chord binding.description (spawn binding.command)
    ) cfg.bindings
    ++ lib.mapAttrsToList (
      chord: binding: key "lock" chord binding.description (spawn binding.command)
    ) (lib.filterAttrs (_: binding: binding.whenLocked) cfg.bindings)
    # Super+F11 toggles the passthrough mode. The mode has no other bindings.
    # Then a nested compositor or a virtual machine gets all keys.
    ++ [
      (key "default" "Super F11" "Passthrough: all keys go to the window" {
        switch_mode.mode = "passthrough";
      })
      (key "passthrough" "Super F11" null { switch_mode.mode = "default"; })
    ];

  floatRule =
    rule:
    lib.optionalAttrs (rule.appId != null) { app_id.str = rule.appId; }
    // lib.optionalAttrs (rule.title != null) { title.str = rule.title; }
    // {
      floating = true;
    };

  padding = 2 * colors.gap;

  # The two 3D edges of win98 and 1 pixel of the face color outside them (no
  # outline).
  borderWidth = 3;
  # kwm measures the gaps between the window contents, and the borders are in
  # the gaps. These gaps keep the space of the borders of 2 pixels: 4 pixels
  # between two borders, and 6 pixels between a border and the edge.
  innerGap = padding + 2 * (borderWidth - 2);
  outerGap = padding + (borderWidth - 2);

  kwmConfig = {
    startup_cmds = [ [ "${startup}" ] ];

    sloppy_focus = true;
    # One tiled window, or the monocle layout, fills the screen without gaps.
    smart_gaps = true;
    cursor_warp = zon.enum "none";
    auto_swallow = false;
    # kwm always asks for server side decorations; only a client without
    # xdg-decoration (GTK3, GTK4) draws its own. A window with server side
    # decorations that is not maximized gets a title bar with a close button,
    # in the colors of its GTK theme (win-classic). Refer to decoration.zig in
    # kwm/.
    default_window_decoration = zon.enum "ssd";
    title_bar = true;
    # A click on the close button 2 seconds after the close request asks to
    # kill the process of a window that does not close (a program that hangs).
    kill_prompt = [
      (lib.getExe cfg.rofi.package)
      "-dmenu"
      "-i"
      "-no-custom"
      "-format"
      "i"
      "-p"
      "{title}"
    ];
    # Super down alone for 2 seconds: a band at the bottom of the focused
    # output with the key bindings and their descriptions (`kwm bindings`).
    # The colors are the tooltip colors of the GTK theme (win-classic), and
    # the chords have the color of the active title bar. Refer to overlay.zig
    # in kwm/.
    overlay = {
      enabled = true;
      command = [
        (lib.getExe kwm)
        "-c"
        "{config}"
        "bindings"
      ];
      delay = 2000;
      # Terminus at 12 pixels has no bold face. The chords are bold.
      font = "Iosevka:size=10";
    };
    # The color of the desktop, below the windows: desktop_color of
    # xfwm4/themerc of the current GTK theme (win-classic). kwm follows each
    # change of toggle-theme. Without that color, the color below.
    background = rgba colors.desktop;
    background_gtk_color = "desktop_color";

    # All bar options of kwm are here, with the default values of
    # config.def.zon. The changes are the colors, the position, the font, the
    # layout clicks and the status.
    bar = {
      show_default = true;
      position = zon.enum "top";
      # A fontconfig pattern for fcft. kwm uses
      # "monospace" for glyphs that Terminus does not have, for example braille.
      #
      # A size in points, not in pixels: kwm loads the font of each bar with the
      # DPI of its output, 96 times the scale. Thus 9 points give 12 pixels at
      # scale 1 and 24 pixels at scale 2. Terminus has these bitmap sizes.
      font = "Terminus:size=9";
      # The tray icons are 16 px, with 2 px above and below. The text is in
      # the vertical center.
      min_height = colors.iconSize + 4;
      scheme = {
        # The whole bar has one background color, black. A brighter text color
        # shows the selected tag and the title of the focused output.
        normal = {
          fg = rgba colors.bar.foreground;
          bg = rgba colors.bar.background;
        };
        select = {
          fg = rgba colors.bar.selected;
          bg = rgba colors.bar.background;
        };
      };

      tags = {
        tags = map toString (lib.range 1 9);
        # Tags without windows are dim.
        empty_fg = rgba colors.bar.emptyTag;
        # The selected tag has the color of the desktop.
        select_bg_desktop = true;
        # kwm puts the clicked tag in place of 0.
        click = {
          left.set_output_tag.tag.tag = 0;
          right.toggle_output_tag.mask = 0;
          middle.toggle_window_tag.mask = 0;
        };
      };

      mode = {
        tags = [
          [
            "default"
            ""
          ]
          [
            "passthrough"
            "P"
          ]
          [
            "floating"
            "F"
          ]
        ];
        click.left.switch_mode.mode = "default";
      };

      layout = {
        # kwm puts the number of managed windows in place of {{?}}.
        tags = {
          tile = {
            left = "[]=";
            right = "=[]";
            top = "[^]";
            bottom = "[_]";
          };
          grid = {
            horizontal = "|+|";
            vertical = "|||";
          };
          monocle = "[{{=}}]";
          deck = {
            left = "[{{D}}]=";
            right = "=[{{D}}]";
            top = "[{{D}}^]";
            bottom = "[{{D}}_]";
          };
          scroller = "[==]";
          centered_master = {
            horizontal = "=[]=";
            vertical = "=[V]=";
          };
          float = "><>";
        };
        # cycle_layout is an action of the kwm fork. A click goes to the next
        # layout, and a right click goes to the previous layout.
        click = {
          left.cycle_layout = forward;
          right.cycle_layout = reverse;
        };
      };

      title.click.left.zoom.swap = false;

      # The widgets of the kwm fork take the place of the status text.
      status = null;

      # The widgets of the kwm fork. Refer to src/config/widget.zig in kwm.
      inherit (cfg.bar) center right end;
      widget_colors = {
        warning = rgba colors.warning;
        critical = rgba colors.critical;
      };
      # The tooltips show at once.
      tooltip_delay = 0;

      # The system tray at the right end. Refer to src/kwm/tray.zig in kwm/.
      tray = {
        icon_size = colors.iconSize;
        # SE98 and Chicago95 have symbolic names linked to regular icons.
        # Use the Papirus-only symbolic theme for the tray.
        icon_theme = "SE98-Papirus-Symbolic";
        spacing = colors.spacing;
        # The menus of the items in rofi, at the top right corner of the
        # screen below the bar. kwm replaces {title} and {y} (the bottom of
        # the bar). These menus are not modal.
        menu_command = [
          (lib.getExe cfg.rofi.package)
          "--no-modal"
          "-dmenu"
          "-i"
          "-no-custom"
          "-format"
          "i"
          "-p"
          "{title}"
          "-show-icons"
          "-icon-theme"
          "SE98-Papirus-Symbolic"
          "-theme-str"
          "window { location: north east; anchor: north east; x-offset: 0px; y-offset: {y}px; width: 360px; }"
        ];
      };

      # The default gives other colors to the mode label. Our colors stay.
      override_colors = [ ];
    };

    # The accent color shows the focused window.
    border = {
      width = borderWidth;
      color = {
        focus = rgba colors.border.focus;
        unfocus = rgba colors.border.unfocus;
        swallowing = rgba colors.border.swallowing;
      };
      # GTK does not use xdg-decoration, thus a GTK window draws its own frame.
      # The win98 frame of GTK is 4 pixels: the two edges, then 2 pixels of
      # face. The margin of 2 pixels keeps 1 pixel for the outline outside the
      # frame. Without the outline (below), kwm makes such a window 1 pixel
      # smaller on each side, thus the frame of GTK fills the place of the
      # border of 3 pixels and 1 pixel more, and its outside edge is where
      # kwm draws the outside edge of a border. kwm draws nothing around it.
      csd_margin = 5 - borderWidth;
      # The window border style of Windows. Refer to raised_border.zig in kwm/.
      # kwm reads the 3D edges of win98 from xfwm4/themerc of the current GTK
      # theme, and follows each change of toggle-theme (the settings portal).
      # No window has an outline: the title bar shows the focus, the title
      # bar of kwm or the title bar of GTK.
      style = zon.enum "raised";
      raised.outline = zon.enum "none";
    };

    # A window of a sandbox (a process in sandbox-<name>.slice) shows the
    # sandbox name in the bar, in a label at its top right corner and in its
    # title bar, in the sandbox color. A window on the host has no label, only
    # "host" in the bar. Refer to sandbox.zig in kwm/.
    sandbox.label = true;

    default_layout = zon.enum "tile";
    layout.tile = {
      nmaster = 1;
      mfact = 0.6;
      inner_gap = innerGap;
      outer_gap = outerGap;
      master_location = zon.enum "left";
    };

    bindings = {
      repeat_info = cfg.keyRepeat;
      key = keyBindings;
      pointer = [
        {
          mode = "default";
          button = zon.enum "left";
          modifiers.mod4 = true;
          event.pressed = zon.enum "pointer_move";
        }
        {
          mode = "default";
          button = zon.enum "right";
          modifiers.mod4 = true;
          event.pressed = zon.enum "pointer_resize";
        }
      ];
    };

    # kwm uses the first rule that matches.
    window_rules = [
      # Zed prefers client side decorations, but its frame is not the win98
      # frame. Zed can also use server side decorations, thus kwm draws the
      # border.
      {
        app_id.str = "dev.zed.Zed";
        decoration = zon.enum "ssd";
      }
    ]
    ++ map floatRule cfg.floatRules
    # Ghostty with window-decoration = none draws no decorations. It does not
    # use xdg-decoration, thus river tells kwm that it draws its own
    # decorations. This rule gives it the border of kwm. The rule is after the
    # float rules: a float rule can find a Ghostty window that draws its own
    # decorations.
    ++ [
      {
        app_id.str = "com.mitchellh.ghostty";
        decoration = zon.enum "ssd";
      }
      # GTK gives the app-id of Ghostty only when Ghostty connects to a session
      # bus. Without a bus (for example in a sandbox), the app-id is "GTK
      # Application". Then kwm makes space for a GTK frame that Ghostty does
      # not draw, and a gap shows between the window and its border. The
      # title of Ghostty is "Ghostty" when its configuration sets title =
      # Ghostty. Then this rule finds Ghostty without the app-id.
      {
        title.str = "Ghostty";
        decoration = zon.enum "ssd";
      }
    ];

  };

  # kwm starts kwim without arguments, each time that an input device comes.
  # Then kwim reads $XDG_CONFIG_HOME/kwim/config.zon, not the kwm configuration.
  # Refer to hotplug_inputs in src/kwm/context.zig of kwm.
  kwimConfig = {
    input_device_rules = [
      {
        repeat_info = cfg.keyRepeat;
      }
    ];
    libinput_device_rules = [
      {
        tap = zon.enum "enabled";
        natural_scroll = zon.enum (if cfg.naturalScroll then "enabled" else "disabled");
        disable_while_typing = zon.enum "enabled";
      }
    ];
    xkb_keyboard_rules = [ { keymap.options = keymap; } ];
  };

  # kwm reads this file only at its start. The unit kwm.service names the file,
  # thus a change of the file changes the unit, and Home Manager restarts kwm.
  kwmConfigFile = pkgs.writeText "kwm-config.zon" (
    zon.generate (lib.recursiveUpdate kwmConfig cfg.kwm.settings)
  );

  # The keymap of all keyboards. river gives its default keymap to each new
  # keyboard, for example to a USB keyboard after a resume. XKB_DEFAULT_* sets
  # the default keymap. kwim sets the same keymap again some time after the
  # new keyboard comes.
  keymap = cfg.keymap;

  # Each river session runs the startup programs one time. river/init makes
  # this marker. kwm runs startup_cmds at each start: at the start of the
  # session, at "systemctl --user restart kwm", and in a nested river. Only the
  # first run in a session finds the marker. Thus a restart of kwm does not
  # start mzde.sessionTarget again, and a nested river does not change the
  # current session.
  startupMarker = "$XDG_RUNTIME_DIR/river5-startup-$WAYLAND_DISPLAY";

  # kwm starts this script. river closes a layer-shell surface if the window
  # manager does not bind river_layer_shell_v1 yet. kwm runs startup_cmds after
  # it binds the river globals. Thus the session programs do not race kwm.
  startup = pkgs.writeShellScript "river5-startup" ''
    marker="${startupMarker}"
    [ -e "$marker" ] || exit 0
    rm -f "$marker"

    ${lib.concatMapStringsSep "\n" (command: "${command} &") cfg.startupCommands}

    # Starts mzde.sessionTarget, and thus the session programs.
    exec ${cfg.sessionHook}/bin/mz-after-systemd
  '';

  # kwm.service runs kwm. Thus "systemctl --user restart kwm" starts a new kwm,
  # for example after ./apply.sh. river continues to operate without a window
  # manager, and it gives the windows to the new kwm.
  init = pkgs.writeShellScript "river-init" ''
    export XDG_CURRENT_DESKTOP=river

    # kwm.service gets its environment from the systemd user manager. Give it
    # WAYLAND_DISPLAY, DISPLAY and PATH. kwm gives this environment to the
    # programs that it starts.
    ${pkgs.dbus}/bin/dbus-update-activation-environment --systemd --all
    ${pkgs.systemd}/bin/systemctl --user import-environment PATH

    touch "${startupMarker}"

    # river sends SIGTERM to the process group of this script when river stops.
    # Then stop kwm and the programs that kwm started: the programs in the
    # cgroup of kwm.service, and the programs that `sandbox host` started
    # in the scopes app-sandbox-host-*.scope (the `sandbox host` command of
    # nix-config).
    stop() {
      ${pkgs.systemd}/bin/systemctl --user kill --kill-whom=all kwm.service
      ${pkgs.systemd}/bin/systemctl --user stop kwm.service 'app-sandbox-host-*.scope'
      exit 0
    }
    trap stop TERM INT HUP

    ${pkgs.systemd}/bin/systemctl --user restart kwm.service

    sleep infinity &
    wait
  '';
in
{
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ river ];
    # The display manager starts the "river" session from river.desktop.
    services.displayManager.sessionPackages = [ river ];
    # river reads XKB_DEFAULT_* when it starts. Refer to keymap above.
    environment.sessionVariables = {
      XKB_DEFAULT_LAYOUT = keymap.layout;
      XKB_DEFAULT_VARIANT = keymap.variant;
      XKB_DEFAULT_OPTIONS = keymap.options;
    };

    # The fonts of the bar (Terminus, and the Nerd Font symbols for the icons
    # of the widgets) and of the overlay of the Super key (Iosevka).
    fonts.packages = [
      pkgs.terminus_font
      pkgs.nerd-fonts.symbols-only
      pkgs.iosevka-bin
    ];

    # The NixOS part of win-classic-theme builds the schemes. Refer to the
    # same option in the Home Manager part below.
    programs.win-classic-theme.kwm.enable = true;

    # The tray programs (pasystray, blueman, udiskie, nm-applet) start after
    # tray.target. kwm is the StatusNotifierWatcher of the tray.
    systemd.user.targets.tray = {
      description = "Tray target for home manager units";
      after = [ "kwm.service" ];
    };

    home-manager.sharedModules = [
      {
        home.packages = [
          kwm
          # kwm finds kwim through PATH.
          kwim
          # Some programs need it to show an item in the tray.
          # https://bbs.archlinux.org/viewtopic.php?id=286513
          pkgs.libappindicator-gtk3
        ];

        xdg.configFile."river/init".source = init;

        systemd.user.services.kwm = {
          Unit = {
            Description = "kwm, the window manager of river";
            X-Restart-Triggers = [ "${kwmConfigFile}" ];
          };
          Service = {
            ExecStart = lib.getExe kwm;
            # The programs that kwm starts stay in the cgroup of this service.
            # KillMode=process stops only kwm. Thus a restart of kwm does not
            # stop the programs.
            KillMode = "process";
            Restart = "on-failure";
            RestartSec = 1;
          };
        };
        xdg.configFile."kwm/config.zon".source = kwmConfigFile;
        xdg.configFile."kwim/config.zon".text = zon.generate kwimConfig;

        # Each win-classic scheme hides the default title bar of a window that
        # kwm maximizes. Refer to kwm.enable in options.nix of
        # win-classic-theme.
        programs.win-classic-theme.kwm.enable = true;
      }
    ];
  };
}
