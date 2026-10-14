{
  lib,
  pkgs,
  config,
  mzdeInputs,
  ...
}:
# The screen lock: swayidle locks the session with waylock, turns off the
# outputs and suspends the system after an idle time.
let
  cfg = config.mzde;
  inherit (cfg) colors;
  # The fork of waylock: a password prompt in Terminus in the center of each
  # output. swayidle with the patch of pkgs/default.nix.
  inherit
    (import ../pkgs {
      inherit pkgs;
      inputs = mzdeInputs;
    })
    waylock
    swayidle
    ;
  # waylock takes the colors as 0xRRGGBB.
  hex = color: "0x${lib.removePrefix "#" color}";

  # waylock fills each output with one color: black when the session locks,
  # the desktop color after a key press, red after a wrong password. It shows
  # no image. The prompt is in the center, in Terminus (the default -font).
  # -fork-on-lock returns after the lock, as swaylock -f did: a child of
  # waylock holds the lock until the user unlocks.
  #
  # After the unlock, the script runs mzde.lock.afterUnlock. The outputs can
  # change while the screen is locked, for example after a resume or when a
  # monitor wakes up late. A lock that fails (the session is locked already)
  # does not run it.
  waylock-wrapper = pkgs.writeShellScriptBin "waylock-wrapper" (
    ''
      ${waylock}/bin/waylock -fork-on-lock \
        -init-color 0x000000 \
        -input-color ${hex colors.desktop} \
        -fail-color ${hex colors.lockFail} || exit
    ''
    + lib.optionalString (cfg.lock.afterUnlock != null) ''

      locker=$(${pkgs.procps}/bin/pgrep -n -x waylock) || exit 0
      (
        ${pkgs.coreutils}/bin/tail --pid="$locker" -f /dev/null
        ${cfg.lock.afterUnlock}
      ) </dev/null >/dev/null 2>&1 &
    ''
  );

  # the following command uses the output of sway-audio-idle-inhibit to control a systemd-inhibit inhibition
  audio-inhibit-service = pkgs.writeShellScriptBin "audio-inhibit-service" ''
    ${pkgs.sway-audio-idle-inhibit}/bin/sway-audio-idle-inhibit --dry-print-sink |
    while read -r status; do
       if [[ $last_status == $status ]]; then
          continue
       fi
       last_status="$status"
       pids="$(${pkgs.procps}/bin/pgrep -d , -f "systemd-inhibit --who=audio-inhibit-service")"
       if [[ $status == "RUNNING" && -z $pids ]]; then
          ${pkgs.systemd}/bin/systemd-inhibit --who=audio-inhibit-service ${pkgs.coreutils}/bin/sleep infinity &> /dev/null &
       fi
       if [[ $status == "NOT RUNNING" && -n $pids ]]; then
          ${pkgs.procps}/bin/pkill -P "$pids"
       fi
    done
  '';
in
{
  config = lib.mkIf cfg.enable {
    security = {
      pam.services = {
        # waylock authenticates with the PAM service "waylock".
        waylock = { };
      };
    };

    environment = {
      systemPackages = [
        waylock
      ];
    };

    mzde.bindings."Super+Control Q" = {
      command = "${pkgs.systemd}/bin/loginctl lock-session";
      description = "Lock the screen";
    };

    home-manager.sharedModules = [
      {
        home.packages = [
          waylock-wrapper
          audio-inhibit-service
          pkgs.sway-audio-idle-inhibit
        ];

        # Audio that plays blocks the idle timers.
        systemd.user.services.audio-idle-inhibit = {
          Unit = {
            Description = "audio-idle-service";
            After = [ cfg.sessionTarget ];
          };

          Service = {
            ExecStart = "${audio-inhibit-service}/bin/audio-inhibit-service";
            Restart = "on-failure";
            RestartSec = 5;
          };

          Install = {
            WantedBy = [ "default.target" ];
          };
        };

        # IMPORTANT!  swayidle only locks the screen and session
        #             logind manages suspension considering the inhibitions
        services.swayidle = {
          enable = true;
          package = swayidle;
          systemdTargets = [ cfg.sessionTarget ];
          # -w: swayidle waits for the command of before-sleep. It holds the
          # delay inhibitor of logind until then, thus the system sleeps only
          # after the lock. waylock-wrapper returns after waylock locked the
          # session (-fork-on-lock). `loginctl lock-session` returns at once,
          # and the lock then came after the sleep. When the session is locked
          # already, waylock-wrapper fails and the sleep continues.
          extraArgs = [
            "-w"
            "idlehint"
            "600"
          ];
          events = {
            before-sleep = "${waylock-wrapper}/bin/waylock-wrapper";
            lock = "${waylock-wrapper}/bin/waylock-wrapper";
          };
          timeouts = [
            {
              timeout = cfg.lock.lockAfter;
              command = "${waylock-wrapper}/bin/waylock-wrapper";
            }
            {
              timeout = cfg.lock.screenOffAfter;
              command = "${pkgs.wlopm}/bin/wlopm --off '*'";
              resumeCommand = "${pkgs.wlopm}/bin/wlopm --on '*'";
            }
          ]
          ++ lib.optional (cfg.lock.sleepAfter != null) {
            timeout = cfg.lock.sleepAfter;
            command = "${pkgs.systemd}/bin/systemctl sleep";
          };
        };
      }
    ];
  };
}
