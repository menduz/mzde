{
  config,
  lib,
  pkgs,
  ...
}:
# The session: the target that starts the session programs, the script that
# starts it, and the media keys.
let
  cfg = config.mzde;

  mz-after-systemd = pkgs.writeShellScriptBin "mz-after-systemd" ''
    # Import the WAYLAND_DISPLAY env var from the compositor into the systemd user session.
    ${pkgs.dbus}/bin/dbus-update-activation-environment --systemd --all

    ${pkgs.systemd}/bin/systemctl --user import-environment PATH

    # this must run BEFORE ${cfg.sessionTarget}
    ${pkgs.systemd}/bin/systemctl --user start xdg-autostart-if-no-desktop-manager.target

    ${pkgs.systemd}/bin/systemctl --user stop ${cfg.sessionTarget}
    ${pkgs.systemd}/bin/systemctl --user start ${cfg.sessionTarget}
  '';
  mz-set-volume = pkgs.writeShellScriptBin "volume-up" ''
    if [[ "$1" = "0" ]]; then
        wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle
    elif [[ "$1" =  "+" ]]; then
        wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%+ --limit 0.8
    elif [[ "$1" = "-" ]]; then
        wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%- --limit 0.8
    fi

    volume_src=$(wpctl get-volume @DEFAULT_AUDIO_SOURCE@)

    volume=$(wpctl get-volume @DEFAULT_AUDIO_SINK@)
    volumep=$(echo $(wpctl get-volume @DEFAULT_AUDIO_SINK@ | tr -dc '0-9')| sed 's/^0*//')
    if [[ -z $volumep ]]; then
        volumep="0"
    elif [[ $volumep -gt "150" ]];then
        volumep="150"
        wpctl set-volume @DEFAULT_AUDIO_SINK@ 1
    fi
    if [[ "$volume" == *"MUTED"* ]]; then
        ${pkgs.libnotify}/bin/notify-send \
          -r 69 \
          -a "Volume" \
          -h int:value:"''${volumep}" \
          -t 888 \
          -u low \
          "MUTED 🔊 Currently at ''${volumep}"
    else
        ${pkgs.libnotify}/bin/notify-send \
          -r 69 \
          -a "Volume" \
          -h int:value:"''${volumep}" \
          -t 888 \
          -u low \
          "🔊 Currently at ''${volumep}"
    fi
  '';

  # Runs mzde.lidSwitch.close and mzde.lidSwitch.open when the lid state
  # changes. UPower sends LidIsClosed in PropertiesChanged.
  hasLidSwitch = cfg.lidSwitch.close != null || cfg.lidSwitch.open != null;
  mz-lid-switch = pkgs.writeShellScriptBin "mz-lid-switch" ''
    ${pkgs.glib}/bin/gdbus monitor --system \
      --dest org.freedesktop.UPower --object-path /org/freedesktop/UPower |
      while IFS= read -r line; do
        case "$line" in
          *"'LidIsClosed': <true>"*)
            echo "lid closed"
            ${lib.optionalString (cfg.lidSwitch.close != null) cfg.lidSwitch.close}
            ;;
          *"'LidIsClosed': <false>"*)
            echo "lid open"
            ${lib.optionalString (cfg.lidSwitch.open != null) cfg.lidSwitch.open}
            ;;
        esac
      done
  '';

  # The media keys: the command and the description.
  media = {
    "None XF86AudioRaiseVolume" = [
      "${lib.getExe mz-set-volume} +"
      "Volume up"
    ];
    "None XF86AudioLowerVolume" = [
      "${lib.getExe mz-set-volume} -"
      "Volume down"
    ];
    "None XF86AudioMute" = [
      "${lib.getExe mz-set-volume} 0"
      "Mute"
    ];

    "None XF86AudioMedia" = [
      "playerctl play-pause"
      "Play or pause"
    ];
    "None XF86AudioPlay" = [
      "playerctl play-pause"
      "Play or pause"
    ];
    "None XF86AudioPrev" = [
      "playerctl previous"
      "Previous track"
    ];
    "None XF86AudioNext" = [
      "playerctl next"
      "Next track"
    ];

    "None XF86MonBrightnessUp" = [
      "brightnessctl set +5%"
      "Brightness up"
    ];
    "None XF86MonBrightnessDown" = [
      "brightnessctl set 5%-"
      "Brightness down"
    ];
  };
in
{
  config = lib.mkMerge [
    { mzde.sessionHook = mz-after-systemd; }
    (lib.mkIf cfg.enable {
      # river has its own Xwayland. Qt programs and the gtk2 platform theme of Qt
      # need it.
      programs.xwayland.enable = lib.mkForce false;

      # The media keys also work while the session is locked.
      mzde.bindings = lib.mapAttrs (_: entry: {
        command = lib.elemAt entry 0;
        description = lib.elemAt entry 1;
        whenLocked = true;
      }) media;

      services.upower.enable = lib.mkIf hasLidSwitch (lib.mkDefault true);

      environment.systemPackages = [
        pkgs.playerctl
        pkgs.brightnessctl
      ];

      home-manager.sharedModules = [
        {
          home.packages = [
            mz-after-systemd
            mz-set-volume
          ];

          # make sure xdg starts after river-session.target
          systemd.user.targets.river-session = {
            Unit = {
              Description = "river compositor session";
              Documentation = [ "man:systemd.special(7)" ];
              BindsTo = [ "graphical-session.target" ];
              Wants = [ "graphical-session-pre.target" ];
              After = [ "graphical-session-pre.target" ];
              PartOf = [ "graphical-session.target" ];
              Before = [
                "graphical-session.target"
                "xdg-desktop-autostart.target"
              ];
            };
          };

          systemd.user.services.mz-lid-switch = lib.mkIf hasLidSwitch {
            Unit = {
              Description = "Run the lid switch commands of mzde";
              PartOf = [ cfg.sessionTarget ];
              After = [ cfg.sessionTarget ];
            };
            Service = {
              ExecStart = lib.getExe mz-lid-switch;
              Restart = "always";
              RestartSec = 5;
            };
            Install.WantedBy = [ cfg.sessionTarget ];
          };
        }
      ];
    })
  ];
}
