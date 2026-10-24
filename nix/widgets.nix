{
  pkgs,
  lib,
  config,
  ...
}:
# The widgets of the kwm bar: the meters of kwm (memory, clock, CPU,
# temperature, battery, disk usage) and script widgets. Each script writes
# JSON lines with "text", "tooltip" and "class", the format of waybar custom
# modules. Refer to src/config/widget.zig in kwm.
#
# Other modules add widgets with mzde.bar.center, mzde.bar.right and
# mzde.bar.end. Refer to options.nix.
let
  cfg = config.mzde;
  zon = import ./zon.nix { inherit lib; };

  btop = "${cfg.terminal} -e ${pkgs.btop}/bin/btop --preset 2 -u 100";

  # The kwm widget "eye" (U+F06E, fa-eye of Font Awesome, also in the Nerd
  # Font symbols): it shows when a lock blocks sleep. The tooltip is the
  # list of locks. A "delay" lock does not block sleep. NetworkManager and
  # UPower always have one.
  sleepInhibitStatus = pkgs.writeShellApplication {
    name = "sleep-inhibit-status";
    runtimeInputs = [
      config.systemd.package
      pkgs.jq
    ];
    text = ''
      if systemd-inhibit --list --no-pager --json=short \
        | jq -e 'any(.[]; .mode == "block" and any(.what | split(":")[]; . == "sleep"))' >/dev/null; then
        systemd-inhibit --list --no-pager | jq -Rsc '{text: " \uf06e ", tooltip: rtrimstr("\n")}'
      else
        echo '{"text": ""}'
      fi
    '';
  };

  # The kwm docker widget: the number of containers that run. It does not
  # show when no container runs or when docker does not answer.
  dockerStatus = pkgs.writeShellApplication {
    name = "docker-status";
    runtimeInputs = [ pkgs.docker ];
    text = ''
      count=$(docker info --format '{{json .ContainersRunning}}' 2>/dev/null || true)
      if [[ "$count" =~ ^[0-9]+$ ]] && [ "$count" -gt 0 ]; then
        printf '{"text": " %s"}\n' "$count"
      else
        echo '{"text": ""}'
      fi
    '';
  };

  lazydocker = "${cfg.terminal} -e ${pkgs.lazydocker}/bin/lazydocker";

  # The commands of the kwm media widget.
  #   - "mpris-ctl halt": when players play, pause all of them and keep their
  #     names. Else resume one player: the first kept player that is still
  #     there, else the default player of playerctl.
  #   - "mpris-ctl toggle <player>": play or pause one player.
  # playerctld is not a player. It sends the commands to an other player.
  mprisCtl = pkgs.writeShellApplication {
    name = "mpris-ctl";
    runtimeInputs = [
      pkgs.playerctl
      pkgs.gnugrep
    ];
    text = ''
      halted="''${XDG_RUNTIME_DIR:-/tmp}/kwm-mpris-halted"

      players() {
        playerctl -l 2>/dev/null | grep -vx playerctld || true
      }

      case ''${1:-} in
        halt)
          playing=()
          while read -r player; do
            if [ "$(playerctl -p "$player" status 2>/dev/null)" = Playing ]; then
              playing+=("$player")
            fi
          done < <(players)

          if [ ''${#playing[@]} -gt 0 ]; then
            for player in "''${playing[@]}"; do
              playerctl -p "$player" pause || true
            done
            printf '%s\n' "''${playing[@]}" >"$halted"
            exit 0
          fi

          kept=()
          if [ -f "$halted" ]; then
            mapfile -t kept <"$halted"
            rm -f "$halted"
          fi
          present=$(players)
          for player in "''${kept[@]}"; do
            if grep -qxF "$player" <<<"$present"; then
              exec playerctl -p "$player" play
            fi
          done
          exec playerctl play
          ;;
        toggle)
          exec playerctl -p "$2" play-pause
          ;;
        *)
          echo "usage: mpris-ctl halt | toggle <player>" >&2
          exit 2
          ;;
      esac
    '';
  };

  # The kwm media widget, as the mpris module of waybar. The text is "▶" when
  # a player plays, else "⏸". The widget does not show without players. The
  # tooltip has a line for each player: the status, the name and
  # "artist - title". A click on a line plays or pauses that player.
  # "playerctl --follow" writes a line for each change. Then the script reads
  # all players again.
  mprisStatus = pkgs.writeShellApplication {
    name = "mpris-status";
    runtimeInputs = [
      pkgs.playerctl
      pkgs.jq
    ];
    text = ''
      # kwm stops the script with SIGTERM. Stop playerctl too.
      trap 'trap - TERM EXIT; kill 0 2>/dev/null' TERM EXIT

      sep=$'\x1f'
      format="{{playerInstance}}$sep{{playerName}}$sep{{status}}$sep{{artist}}$sep{{title}}"

      status() {
        { playerctl -a metadata --format "$format" 2>/dev/null || true; } | jq -Rsc --arg ctl "${lib.getExe mprisCtl}" '
          def icon: if .status == "Playing" then "▶" elif .status == "Paused" then "⏸" else "■" end;
          def track:
            [.artist, .title | select(. != "")] | join(" - ")
            | if length > 60 then .[:59] + "…" else . end;
          [ split("\n")[] | select(. != "") | split("\u001f")
            | { player: .[0], name: .[1], status: .[2], artist: (.[3] // ""), title: (.[4] // "") }
            | select(.player != "playerctld") ] as $players
          | if $players == [] then {text: ""}
            else
              ($players | map(.name | length) | max) as $width
              | {
                  text: (if any($players[]; .status == "Playing") then "▶" else "⏸" end),
                  tooltip: ($players | map("\(icon) \(.name)\(" " * ($width - (.name | length) + 2))\(track)") | join("\n")),
                  tooltip_on_click: ($players | map("\($ctl) toggle \(.player | @sh)"))
                }
            end'
      }

      last=""
      { echo; playerctl -a --follow metadata --format "$format" 2>/dev/null; } | while read -r _; do
        line=$(status)
        if [ "$line" != "$last" ]; then
          last=$line
          printf '%s\n' "$line"
        fi
      done
    '';
  };

  # The kwm privacy widget: a microphone and a camera that flash red while a
  # program uses them.
  #   - Microphone: the PipeWire capture streams (Stream/Input/Audio) that
  #     run. The monitor streams and the peak meters of pavucontrol do not
  #     count.
  #   - Camera: the PipeWire cameras and video streams that run, and the
  #     programs that open /dev/video* themselves (most browsers). An open or
  #     a close of a video device starts a search in /proc.
  # pw-dump --monitor and inotifywait wait for events, so the script uses no
  # CPU while nothing changes. kwm starts the script again when it stops.
  privacyStatus = pkgs.writeShellApplication {
    name = "privacy-status";
    runtimeInputs = [
      pkgs.pipewire
      pkgs.jq
      pkgs.inotify-tools
      pkgs.findutils
      pkgs.coreutils
      pkgs.gnused
    ];
    text = ''
      # kwm stops the script with SIGTERM. Stop the pipelines too.
      trap 'trap - TERM EXIT; kill 0 2>/dev/null' TERM EXIT

      mic_icon=$'\U000F036C'
      cam_icon=$'\U000F05A0'

      # One line "pw {"mic": [...], "cam": [...]}" for each change in PipeWire.
      pipewire_streams() {
        pw-dump --monitor --no-colors | jq -c --unbuffered -n '
          foreach (inputs | .[]?) as $o ({};
            ($o.id | tostring) as $k
            | if $o.info == null then del(.[$k])
              elif $o.type == "PipeWire:Interface:Node" or has($k) then .[$k] = ((.[$k] // {}) * $o.info)
              else . end;
            [ .[] | select(.state == "running") | .props // {} ] as $running
            | {
                mic: [ $running[]
                       | select(."media.class" == "Stream/Input/Audio")
                       | select((."stream.monitor" // false | tostring) != "true")
                       | select(."media.name" != "Peak detect")
                       | (."application.name" // ."node.name" // "?") ] | unique,
                cam: [ $running[]
                       | select(."media.class" == "Video/Source" or ."media.class" == "Stream/Input/Video")
                       | (."application.name" // ."node.description" // ."node.name" // "?") ] | unique
              })' | sed -u 's/^/pw /'
      }

      # One line "dev" at the start, and for each open or close of a video
      # device.
      video_events() {
        echo dev
        shopt -s nullglob
        local devices=(/dev/video*)
        if [ ''${#devices[@]} -eq 0 ]; then
          sleep infinity
        fi
        inotifywait -m -q -e open -e close --format dev "''${devices[@]}"
      }

      # The programs that have a video device open, without PipeWire.
      video_users() {
        { find /proc/[0-9]*/fd -maxdepth 1 -lname '/dev/video*' 2>/dev/null || true; } \
          | cut -d/ -f3 | sort -u \
          | while read -r pid; do
              comm=$(cat "/proc/$pid/comm" 2>/dev/null) || continue
              case $comm in
                pipewire | wireplumber | pipewire-media-session) ;;
                *) echo "$comm" ;;
              esac
            done \
          | jq -R . | jq -sc 'unique'
      }

      pw='{"mic": [], "cam": []}'
      dev='[]'
      last=""
      # When one source stops, stop all. kwm starts the script again.
      { pipewire_streams & video_events & wait -n; kill 0; } | while read -r kind data; do
        case $kind in
          pw) pw=$data ;;
          dev) dev=$(video_users) ;;
        esac
        state=$(jq -nc --argjson pw "$pw" --argjson dev "$dev" '{mic: $pw.mic, cam: ($pw.cam + $dev | unique)}')
        if [ "$state" = "$last" ]; then
          continue
        fi
        last=$state
        jq -nc --argjson s "$state" --arg mic "$mic_icon" --arg cam "$cam_icon" '
          if ($s.mic | length) + ($s.cam | length) == 0 then {text: ""}
          else {
            text: ([ (if ($s.mic | length) > 0 then $mic else empty end),
                     (if ($s.cam | length) > 0 then $cam else empty end) ] | join(" ")),
            tooltip: ([ (if ($s.mic | length) > 0 then "Microphone: " + ($s.mic | join(", ")) else empty end),
                        (if ($s.cam | length) > 0 then "Camera: " + ($s.cam | join(", ")) else empty end) ] | join("\n")),
            class: ["critical", "blink"]
          } end'
      done
    '';
  };

  # Writes a JSON line (the format of waybar custom modules) each 2 seconds
  # while a disk is busy, and an empty line when the disks become calm.
  # Busy: the part of the time with I/O in progress (io_ticks of
  # /proc/diskstats), the mean of the last two periods (4 seconds). The disks
  # are the whole disks and the device-mapper volumes (an encrypted volume
  # can be busy while its disk is not); a mounted volume shows its mount
  # point. 75% gives the warning color, 95% the critical color.
  busy-status = pkgs.writeShellApplication {
    name = "disk-busy-status";
    text = ''
      period=2
      threshold=75
      critical=95

      declare -A ticks_before sread_before swrite_before busy_before
      time_before=""
      shown=""

      # The label of a device: the mount point of a mounted volume, else the
      # name of the device (for device-mapper, its mapper name).
      label() {
        local name=$1 dev mapper=""
        dev=/dev/$name
        if [ -r "/sys/class/block/$name/dm/name" ]; then
          mapper=$(<"/sys/class/block/$name/dm/name")
          dev=/dev/mapper/$mapper
        fi
        local source target _
        while read -r source target _; do
          if [ "$source" = "$dev" ]; then
            echo "$target"
            return
          fi
        done </proc/mounts
        echo "''${mapper:-$name}"
      }

      while true; do
        now=''${EPOCHREALTIME/./}
        busy_text=() lines=() max=0
        while read -r _ _ name _ _ sread _ _ _ swrite _ _ ticks _; do
          # Whole disks and device-mapper volumes only: no partition (sysfs
          # marks it; without sysfs, its name ends in a number after the disk
          # name), no loop, RAM or optical device.
          case $name in loop* | ram* | zram* | sr* | fd*) continue ;; esac
          if [ -d "/sys/class/block/$name" ]; then
            [ ! -e "/sys/class/block/$name/partition" ] || continue
          elif [[ $name =~ ^(nvme[0-9]+n[0-9]+p|mmcblk[0-9]+p)[0-9]+$ || $name =~ ^[shv]d[a-z]+[0-9]+$ ]]; then
            continue
          fi

          if [ -n "$time_before" ] && [ -n "''${ticks_before[$name]:-}" ]; then
            elapsed_ms=$(((now - time_before) / 1000))
            [ "$elapsed_ms" -gt 0 ] || elapsed_ms=1
            busy=$(((ticks - ticks_before[$name]) * 100 / elapsed_ms))
            [ "$busy" -le 100 ] || busy=100
            mean=$(((busy + ''${busy_before[$name]:-$busy}) / 2))
            busy_before[$name]=$busy
            read_mb=$(((sread - sread_before[$name]) * 512 / 1000 / elapsed_ms))
            write_mb=$(((swrite - swrite_before[$name]) * 512 / 1000 / elapsed_ms))
            lbl=$(label "$name")
            lines+=("$(printf '%-24s %3d%%   read %4d MB/s   write %4d MB/s' "$lbl" "$mean" "$read_mb" "$write_mb")")
            if [ "$mean" -ge "$threshold" ]; then
              busy_text+=("$lbl $mean%")
              [ "$mean" -le "$max" ] || max=$mean
            fi
          fi
          ticks_before[$name]=$ticks
          sread_before[$name]=$sread
          swrite_before[$name]=$swrite
        done </proc/diskstats
        time_before=$now

        if [ ''${#busy_text[@]} -gt 0 ]; then
          class=warning
          [ "$max" -lt "$critical" ] || class=critical
          text="󰋊 $(IFS=' '; echo "''${busy_text[*]}")"
          tooltip=$(printf '%s\\n' "''${lines[@]}")
          printf '{"text": " %s ", "tooltip": "%s", "class": "%s"}\n' "$text" "''${tooltip%\\n}" "$class"
          shown=yes
        elif [ -n "$shown" ] || [ -z "''${started:-}" ]; then
          echo '{"text": ""}'
          shown=""
        fi
        started=yes
        sleep "$period"
      done
    '';
  };

  # Prints one JSON line with "text", "tooltip" and "class", the format of
  # waybar custom modules. The kwm bar shows the text, and the tooltip holds
  # the number of notifications and the last 10 of the history.
  notification-status = pkgs.writeShellScriptBin "notification-status" ''
    history=$(${pkgs.dunst}/bin/dunstctl history 2>/dev/null) || history='{"data":[[]]}'
    printf '%s' "$history" | ${pkgs.jq}/bin/jq -c '
      (.data[0] // []) as $items
      | if ($items | length) == 0 then
          { text: "●", tooltip: "No notifications", class: "" }
        else
          {
            text: "●",
            class: "has-notifications",
            tooltip: (
              [ "\($items | length) notifications" ]
              + [ $items[:10][]
                  | "\(.appname.data): \(.summary.data)"
                    + (if (.body.data // "") == "" then ""
                       else " — " + (.body.data | gsub("\\s+"; " ")) end) ]
              | join("\n")
            )
          }
        end'
  '';
in
{
  config = lib.mkIf cfg.enable {
    # The widgets of mzde. Other widgets without an order go between the
    # busy disks and the privacy widget in the center, after the disk usage
    # at the right, and between the clock and the notifications at the end.
    mzde.bar.center = lib.mkMerge [
      (lib.mkOrder 900 [
        # The disks that are busy 75% of the time or more. Empty when no disk
        # is that busy.
        (cfg.lib.scriptWidget {
          exec = lib.getExe busy-status;
          onClick = btop;
        })
      ])
      # The microphone and the camera flash red while a program uses them.
      # The script writes a line for each change.
      (lib.mkOrder 1100 [
        (cfg.lib.scriptWidget { exec = lib.getExe privacyStatus; })
      ])
    ];
    mzde.bar.right = lib.mkOrder 900 [
      # The temperature of the hottest sensor, for example "41C", from
      # mzde.bar.temperature.show degrees. The tooltip has all the sensors.
      # Options in mzde.bar.temperature.
      {
        temperature = {
          format = zon.enum "celsius";
          on_click = btop;
          inherit (cfg.bar.temperature) ignore show;
          nvidia_smi = cfg.bar.temperature.nvidiaSmi;
          # kwm gives the colors at a percentage of the range from low to
          # high. From 0 to 100 degrees, the percentage is the temperature.
          low = 0;
          high = 100;
          inherit (cfg.bar.temperature) warning critical;
        };
      }
      # An eye when a lock blocks sleep. The tooltip is the list of locks.
      (cfg.lib.scriptWidget {
        exec = lib.getExe sleepInhibitStatus;
        interval = 5000;
      })
      # The media players. A click pauses all players that play, and the
      # next click resumes one of them. A click on a line of the tooltip
      # plays or pauses that player.
      (cfg.lib.scriptWidget {
        exec = lib.getExe mprisStatus;
        onClick = "${lib.getExe mprisCtl} halt";
      })
      # The number of docker containers that run. A click starts lazydocker.
      (cfg.lib.scriptWidget {
        exec = lib.getExe dockerStatus;
        interval = 10000;
        onClick = lazydocker;
      })
      # Shown on battery and while charging.
      {
        battery = {
          warning = 20;
          critical = 10;
        };
      }
      # Shown when 80% of / is used, and red below 10 GB free.
      {
        disk = {
          show = 80;
          alert = 10 * 1000 * 1000 * 1000;
        };
      }
    ];
    mzde.bar.end = lib.mkMerge [
      # At the right of the tray: a gauge with a bar for the memory and a
      # bar for the CPU use, then the clock. The tooltip of the gauge has
      # both values. A click on the gauge starts btop. Refer to
      # src/kwm/gauge.zig in kwm/.
      (lib.mkOrder 900 [
        {
          gauge = {
            bars = [
              (zon.enum "memory")
              (zon.enum "cpu")
            ];
            on_click = btop;
          };
        }
        { clock = { }; }
      ])
      # At the right end: the notification dot of dunst. Its tooltip holds
      # the last notifications. It has no click command.
      (lib.mkOrder 1100 [
        (cfg.lib.scriptWidget {
          exec = lib.getExe notification-status;
          interval = 5000;
        })
      ])
    ];

    home-manager.sharedModules = [ { home.packages = [ notification-status ]; } ];
  };
}
