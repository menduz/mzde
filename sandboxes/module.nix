# Named sandboxes for programs, as the qubes of Qubes OS. Each sandbox has its
# home directory, its color, its network and its capabilities. The `sandbox`
# command starts a program in a sandbox with a backend: bubblewrap, gVisor, or
# a backend of the configuration.
#
# The options of a sandbox have the names of the configuration keys of
# sandboxed-agents. Refer to https://github.com/eordano/sandboxed-agents.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.programs.sandboxes;

  backendPackages = pkgs.callPackage ./backends { sandbox = cfg.package; };

  jsonFormat = pkgs.formats.json { };

  bool =
    default: description:
    mkOption {
      type = types.bool;
      inherit default description;
    };

  # true and false keep the meaning of sandboxed-agents: the host, or nothing.
  serviceMode =
    description:
    mkOption {
      type = types.coercedTo types.bool (b: if b then "host" else "none") (
        types.enum [
          "none"
          "private"
          "host"
        ]
      );
      default = "private";
      inherit description;
    };

  # The access of a sandbox to the configuration of a program on the host.
  # true is "read", as in sandboxed-agents, and false is "none".
  access =
    description:
    mkOption {
      type = types.coercedTo types.bool (b: if b then "read" else "none") (
        types.enum [
          "none"
          "read"
          "write"
        ]
      );
      default = "none";
      description = ''
        ${description}
        - "read" (or true): read-only.
        - "write": writable. The command makes a directory that is not on
          the host, so that the files of the program go to the host.
        - "none" (or false): the sandbox does not get it.
      '';
    };

  # The paths that each access option mounts, below $HOME. A "/" at the end
  # is a directory. A path below .config, .local/share or .cache comes from
  # the XDG directory of the host. With `xdgRemap`, `xdg` is mounted in place
  # of the first path, if it is on the host.
  programConfigs = {
    ssh.paths = [ ".ssh/" ];
    git.paths = [
      ".gitconfig"
      ".config/git/"
      ".git-credentials"
    ];
    claude = {
      paths = [
        ".claude/"
        ".claude.json"
      ];
      xdg = ".config/claude/";
    };
    codex = {
      paths = [ ".codex/" ];
      xdg = ".config/codex/";
    };
    gemini = {
      paths = [ ".gemini/" ];
      xdg = ".config/gemini/";
    };
    hermes = {
      paths = [ ".hermes/" ];
      xdg = ".config/hermes/";
    };
    opencode.paths = [ ".config/opencode/" ];
    # The data directory has the extensions, the database and the socket
    # of the running Zed.
    zed.paths = [
      ".config/zed/"
      ".local/share/zed/"
    ];
    # The configuration (direnvrc) and the list of the allowed .envrc files.
    direnv.paths = [
      ".config/direnv/"
      ".local/share/direnv/"
    ];
  };

  # The access of the sandbox e to the configuration of program.
  accessOf =
    e: program: if program == "ssh" && e.sshWrite && e.ssh != "none" then "write" else e.${program};

  # The mounts of the access options, for the `sandbox` command.
  configMounts =
    e:
    lib.concatLists (
      lib.mapAttrsToList (
        program: c:
        let
          mode = accessOf e program;
        in
        lib.optionals (mode != "none") (
          lib.imap0 (i: path: {
            mode = if mode == "write" then "rw" else "ro";
            inherit path;
            remap = if i == 0 && e.xdgRemap && c ? xdg then c.xdg else "";
          }) c.paths
        )
      ) programConfigs
    );

  # yolo: claude with --dangerously-skip-permissions. The wrapper is at the
  # start of PATH, and it starts the next claude in PATH (`sandbox __yolo`
  # removes the directory of the wrapper from PATH).
  yoloClaude = pkgs.runCommand "sandbox-yolo-claude" { nativeBuildInputs = [ pkgs.makeBinaryWrapper ]; } ''
    makeWrapper ${lib.getExe cfg.package} $out/bin/claude --add-flags "__yolo $out/bin"
  '';

  # "KEY=VALUE" to { name = KEY; value = VALUE; }.
  parseEnv =
    s:
    let
      m = builtins.match "([^=]+)=(.*)" s;
    in
    if m == null then
      throw "programs.sandboxes: extraEnvs: '${s}' is not KEY=VALUE"
    else
      lib.nameValuePair (lib.elemAt m 0) (lib.elemAt m 1);

  strList =
    description:
    mkOption {
      type = types.listOf types.str;
      default = [ ];
      inherit description;
    };

  fileType = types.submodule (
    { name, config, ... }:
    {
      options = {
        text = mkOption {
          type = types.nullOr types.lines;
          default = null;
          description = "The text of the file.";
        };
        source = mkOption {
          type = types.path;
          description = "The file. Set `text` or `source`.";
        };
      };
      config.source = lib.mkIf (config.text != null) (
        lib.mkDefault (pkgs.writeText (baseNameOf name) config.text)
      );
    }
  );

  commandType = types.submodule (
    { name, ... }:
    {
      options = {
        package = mkOption {
          type = types.package;
          description = "The package of the program.";
        };
        program = mkOption {
          type = types.str;
          default = name;
          defaultText = lib.literalExpression "<name>";
          description = "The program in the bin directory of the package.";
        };
        cwdOnly = bool false ''
          Of the `paths` of the sandbox, the program sees only the current
          directory. The other directories of `paths` are empty, and the
          files of `paths` are empty files. The other mounts of the sandbox
          stay. If a path has the configuration of a program (a path ~ with
          claude = "write"), the command does not start: cwdOnly would hide
          it. The program runs in a nested bwrap, thus the sandbox needs user
          namespaces (the bwrap backend).
        '';
        bwrapArgs = strList ''
          More arguments for the nested bwrap of the program, for example
          `[ "--unshare-uts" "--hostname" "work" ]`. With arguments, the
          program runs in a nested bwrap also without `cwdOnly`.
        '';
      };
    }
  );

  # The command NAME: in the sandboxes with the command, `sandbox __command`
  # starts the program; elsewhere, it stops with an error. The data of the
  # commands is in config.json ("commands").
  commandWrapper =
    name:
    pkgs.runCommand "sandbox-command-${name}" { nativeBuildInputs = [ pkgs.makeBinaryWrapper ]; } ''
      makeWrapper ${lib.getExe cfg.package} $out/bin/${lib.escapeShellArg name} \
        --add-flags ${lib.escapeShellArg "__command ${name}"}
    '';

  # The uses of each command name, for commandWrapper.
  commandUses = lib.zipAttrs (
    lib.mapAttrsToList (
      env: e:
      lib.mapAttrs (_: c: {
        inherit env e c;
      }) e.commands
    ) cfg.environments
  );

  envModule =
    { name, config, ... }:
    {
      options = {
        description = mkOption {
          type = types.str;
          default = "";
          description = "A short description. `sandbox list` and the menu show it.";
        };

        color = mkOption {
          type = types.strMatching "#[0-9a-fA-F]{6}";
          default = "#808080";
          description = ''
            The label color of the sandbox. A window manager reads it from
            /etc/sandboxes/config.json and draws the border of the windows of
            the sandbox in this color. The programs in the sandbox get it in
            the SANDBOX_COLOR environment variable, for a shell prompt.
          '';
        };

        gtkTheme = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "Adwaita:dark";
          description = ''
            The GTK_THEME of the programs. The theme must be in a directory of
            XDG_DATA_DIRS, for example in environment.systemPackages.
          '';
        };

        backend = mkOption {
          # "runsc" is the name of gVisor in sandboxed-agents.
          type =
            let
              names = types.enum (lib.attrNames cfg.backends);
            in
            if cfg.backends ? gvisor && !(cfg.backends ? runsc) then
              types.coercedTo (types.enum [ "runsc" ]) (_: "gvisor") names
            else
              names;
          default = cfg.defaultBackend;
          defaultText = lib.literalExpression "programs.sandboxes.defaultBackend";
          description = ''
            The backend that isolates the programs: a name of
            `programs.sandboxes.backends`. The SANDBOX_BACKEND environment
            variable replaces it for one run.
          '';
        };

        backendArgs = mkOption {
          type = types.attrsOf (types.listOf types.str);
          default = { };
          example = {
            gvisor = [ "--debug" ];
          };
          description = ''
            Arguments for each backend, after all other arguments of that
            backend. The name is the name of the backend.
          '';
        };

        hostname = mkOption {
          type = types.str;
          default = name;
          description = "The host name in the sandbox.";
        };

        persistentHome = bool true ''
          Whether the home directory keeps its files. If false, the home
          directory is a tmpfs, and it is empty at each start.
        '';

        homeDirectory = mkOption {
          type = types.str;
          default = "${cfg.stateDir}/${name}/home";
          defaultText = lib.literalExpression ''"''${stateDir}/<name>/home"'';
          description = ''
            The directory on the host that the sandbox sees as $HOME. A "~" at
            the start is the home directory of the user.
          '';
        };

        homePatterns = strList ''
          Paths below $HOME that the sandbox gets from the home directory of
          the host, read-only. A "rw:" prefix makes a path writable.
        '';

        files = mkOption {
          type = types.attrsOf fileType;
          default = { };
          example = lib.literalExpression ''{ ".config/foo/config".text = "a = 1"; }'';
          description = ''
            Files in the home directory of the sandbox, read-only. The name is
            the path below $HOME. A file here replaces the file of
            `homePatterns`.
          '';
        };

        paths = strList ''
          Paths to bind-mount, writable. "ro:/path" is read-only.
          "/host:/guest" mounts /host at /guest. A "~" at the start is the
          home directory of the user. A path that does not exist gives a
          warning.
        '';

        env = mkOption {
          type = types.attrsOf types.str;
          default = { };
          description = "Extra environment variables.";
        };

        extraEnvs = strList ''
          Extra environment variables as "KEY=VALUE", added to `env`. The
          same as `env`, with the format of sandboxed-agents.
        '';

        dconfSettings = mkOption {
          type = types.attrsOf (types.attrsOf types.anything);
          default = { };
          example = {
            "org/gnome/desktop/interface" = {
              font-name = "Inter 9";
              color-scheme = "prefer-dark";
            };
          };
          description = ''
            GSettings values of the sandbox, as the `settings` of a database of
            `programs.dconf.profiles`. GTK on Wayland reads the fonts, the
            cursor and the icon theme from GSettings, not from settings.ini.
            The dconf database of the sandbox home directory comes first, so a
            program in the sandbox can change a value for the sandbox. A plain
            integer is an int32, as in the `dconf.settings` of Home Manager.
          '';
        };

        passEnv = strList ''
          Environment variables that go from the caller into the sandbox,
          added to `programs.sandboxes.passEnv`. All other variables stay out.
        '';

        packages = mkOption {
          type = types.listOf types.package;
          default = [ ];
          description = "Packages at the start of PATH in the sandbox.";
        };

        commands = mkOption {
          type = types.attrsOf (types.coercedTo types.package (package: { inherit package; }) commandType);
          default = { };
          example = lib.literalExpression ''
            {
              claude = pkgs.claude-code;
              zeditor = { package = pkgs.zed-editor; program = "zeditor"; };
            }
          '';
          description = ''
            Programs that run only in sandboxes. The system and the sandbox
            get a command with the name of the attribute. In a sandbox with
            the command, the command starts the program. On the host and in
            the other sandboxes, the command stops with an error. `sandbox
            current` gives the sandbox. The command is at the start of PATH
            in this sandbox. Many sandboxes can have a command with the same
            name; set it in `programs.sandboxes.defaults` for all sandboxes.
          '';
        };

        ssh = access "Mount ~/.ssh. Any value except \"none\" also gives the SSH agent socket.";
        sshWrite = bool false "Mount ~/.ssh writable: the same as `ssh = \"write\"`. Needs `ssh`.";
        gpg = bool false "Mount ~/.gnupg and the sockets of gpg-agent.";
        git = access "Mount the git configuration and the credentials (~/.gitconfig, ~/.config/git, ~/.git-credentials).";
        claude = access "Mount the configuration of Claude Code (~/.claude, ~/.claude.json).";
        codex = access "Mount the configuration of Codex (~/.codex).";
        gemini = access "Mount the configuration of Gemini CLI (~/.gemini).";
        hermes = access "Mount the configuration of Hermes (~/.hermes).";
        opencode = access "Mount the configuration of OpenCode (~/.config/opencode).";
        zed = access "Mount the configuration and the data of Zed (~/.config/zed, ~/.local/share/zed).";
        direnv = access ''
          Mount the configuration of direnv and its list of allowed .envrc
          files (~/.config/direnv, ~/.local/share/direnv). With "read", direnv
          loads the .envrc files that the host allowed, and the sandbox cannot
          allow more.
        '';
        xdgRemap = bool false ''
          Mount $XDG_CONFIG_HOME/<program> of the host at ~/.<program> for
          claude, codex, gemini and hermes. If that directory is not on the
          host, the sandbox gets ~/.<program>.
        '';
        noXdgRemap = bool false "Make `xdgRemap` false, also if a different definition sets it.";
        yolo = bool false ''
          Put a `claude` command first in PATH that starts the next claude in
          PATH with --dangerously-skip-permissions.
        '';
        docker = bool false ''
          Mount the Docker socket. Access to the Docker socket is the same as
          root access to the host.
        '';
        fuse = bool false "Mount /dev/fuse.";
        gui = bool true ''
          Give a Wayland socket, /dev/dri and /sys. The Wayland socket is a
          socket of a security context (wp_security_context_v1), not the
          socket of the compositor: the compositor does not give the
          privileged protocols (virtual keyboard, screen capture, clipboard
          data control) to the programs of the sandbox.
        '';
        x11 = serviceMode ''
          The X server of the sandbox. Needs `gui`.
          - "private": an X server for the sandbox (xwayland-satellite), for
            all runs. X11 clients see no X11 client of the host or of other
            sandboxes.
          - "host": the X sockets of the host. An X11 client can read the
            keyboard and the screen of all other X11 clients, so this removes
            the isolation.
          - "none": no X server.
          true is "host" and false is "none", as in sandboxed-agents.
        '';
        dbus = serviceMode ''
          The D-Bus session bus of the sandbox.
          - "private": a session bus for the sandbox, for all runs. The
            programs of the sandbox can use D-Bus, and they see nothing of
            the host.
          - "host": the session bus of the host. A program with the session
            bus can start programs outside the sandbox, so this removes the
            isolation.
          - "none": no session bus.
          true is "host" and false is "none".
        '';
        notifications = bool true ''
          Show the notifications of the programs of the sandbox with the
          notification server of the host (dunst), with the name of the
          sandbox before the summary and the border in the color of the
          sandbox. Needs `dbus = "private"`. The private bus starts
          `sandbox __notify_bridge` for org.freedesktop.Notifications, never
          the notification server of the host in the sandbox; on the host,
          the container checks each notification (no paths as icons, only
          some hints, no actions: a click only closes the notification, a
          rate limit) and lets the sandbox replace or close only its own
          notifications. false: the programs get an error for each
          notification.
        '';
        nvidia = bool false "Mount the NVIDIA devices.";
        kvm = bool false "Mount /dev/kvm and /dev/vfio.";
        audio = bool false "Mount the PipeWire and PulseAudio sockets and /dev/snd.";
        libvirt = bool false "Mount the libvirt sockets.";
        nix = bool false ''
          Connect the Nix commands of the sandbox to the Nix daemon of the
          host, so that the sandbox uses the store of the host and does not
          download or build again what the host has. The daemon does not
          trust the sandbox, also if the user is in `nix.settings.trusted-users`
          (a trusted client can get root on the host): the user service
          sandbox-nix-daemon@ forwards each connection with
          `nix-daemon --stdio --force-untrusted --process-ops`. Thus the sandbox cannot set
          restricted settings, and it can use only the substituters of the
          host and `trusted-substituters`.

          The builds run outside the sandbox: a fixed-output derivation can
          connect to all hosts that the Nix daemon can connect to.

          The sandbox also gets `~/.cache/nix` of the host, writable: the
          tarball cache, the fetcher cache, the eval cache and the narinfo
          cache. Without it, Nix (and devenv) download again the sources of
          the flake inputs that are in the store. A program of the sandbox can
          write the eval cache of the host, thus change what a `nix build` of a
          flake on the host evaluates to (also `nixos-rebuild`).
        '';

        allowHome = bool false ''
          Mount the real home directory, writable, in place of the home
          directory of the sandbox. This removes most of the isolation.
        '';

        internetAccess = bool true ''
          Allow connections to the internet. If false, only the local networks
          (RFC 1918, CGNAT, link-local) and `allowHosts` are open.
        '';
        lanAccess = bool true ''
          Allow connections to the local networks. If false, only the internet
          is open. This option is not in sandboxed-agents.
        '';
        allowHosts = strList ''
          Hosts that are open when `internetAccess` is false. The command
          resolves the names when the sandbox starts.
        '';
        disableNetworking = bool false "No network: only the loopback interface of the sandbox.";
        shareHostNetwork = bool false ''
          Use the network namespace of the host, without slirp4netns. The
          sandbox can then connect to all sockets of the host on 127.0.0.1,
          and to the abstract Unix sockets (for example Xwayland). This option
          is not in sandboxed-agents.
        '';
        allowHostLoopback = bool false ''
          Allow connections to 127.0.0.1 of the host through 10.0.2.2. This
          option is not in sandboxed-agents.
        '';
        socksProxy = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "socks5://192.168.1.10:1080";
          description = ''
            Send all traffic through this SOCKS5 proxy (tun2socks). Without a
            scheme ("HOST:PORT"), the scheme is socks5. The host of the proxy
            is open also when `internetAccess` or `lanAccess` is false.
          '';
        };
        privacyFilter = bool false ''
          Send all traffic through the privacy-filter proxy of
          sandboxed-agents. The proxy is `socksProxy`, or port 1080 on
          127.0.0.1 of the host (then the sandbox gets `allowHostLoopback`).
          The sandbox must also trust the CA of the proxy: mount it with
          `paths` and set the variables of the CA with `env`.
        '';

        mountHomeCache = bool false ''
          Mount the caches of development tools (`programs.sandboxes.homeCacheDirs`).
        '';
        mountCommonHomeFolders = bool false ''
          Mount the home folders of toolchains (`programs.sandboxes.commonHomeFolders`).
        '';
        mountTmp = bool false "Mount the /tmp of the host in place of a tmpfs.";
        sharedTmp = bool false ''
          Mount /tmp/sandbox-<uid>-<name> of the host at /tmp, in place of a
          tmpfs. The runs in the container of the sandbox share its tmpfs;
          with this option, the files stay when the container stops, and the
          runs without a scope (SANDBOX_NO_SCOPE) get the same /tmp. The host
          removes old files in /tmp (systemd-tmpfiles). `mountTmp` replaces
          this option.
        '';
        mounts = mkOption {
          type = types.listOf (
            types.enum [
              "common-tools"
              "caches"
            ]
          );
          default = [ ];
          description = ''
            The mount groups of sandboxed-agents: "common-tools" sets
            `mountCommonHomeFolders`, and "caches" sets `mountHomeCache`.
          '';
        };
        cleanTmp = bool true ''
          Remove the directory of the run (the plan, the network settings)
          when the program stops. If false, the command keeps it and tells
          its path.
        '';

        opensnitch = {
          enable = bool false ''
            Run an OpenSnitch daemon in the network namespace of the sandbox.
            The daemon sees each connection of the programs of the sandbox
            before slirp4netns, and it asks the OpenSnitch GUI of the host.
            The GUI shows the daemon as the node "unix:/<name>".

            The sandbox has one daemon: the container of the sandbox (the
            user service sandbox@<name>) holds the network namespace,
            slirp4netns, the firewall and the daemon. The service stops after
            the last run. A run without a scope (SANDBOX_NO_SCOPE) gets a
            daemon of its own. Needs a private network (not
            `disableNetworking` or `shareHostNetwork`).
          '';

          rules = mkOption {
            type = types.attrsOf jsonFormat.type;
            default = { };
            description = ''
              OpenSnitch rules for this sandbox only, added to
              `programs.sandboxes.opensnitch.rules`. A rule here replaces a
              rule there with the same name.
            '';
          };

          allowOnHost = bool false ''
            Add a rule to `services.opensnitch.rules`: the daemon of the host
            allows all connections of the slirp4netns of this sandbox, because
            the daemon of the sandbox already asked for them. Without this
            rule, the host asks again for slirp4netns.

            The rule finds the slirp4netns of the sandbox by SANDBOX_NAME in
            its environment. A program on the host can set the same variable,
            start its own slirp4netns, and get past the daemon of the host.
          '';

          directory = mkOption {
            type = types.str;
            default = "${cfg.stateDir}/${name}/opensnitch";
            defaultText = lib.literalExpression ''"''${stateDir}/<name>/opensnitch"'';
            description = ''
              The directory of the daemon on the host: its configuration and
              its rules. The rules of the configuration are links into the
              store; the rules of the GUI are files, and they stay. A "~" at
              the start is the home directory of the user. The sandbox does
              not see this directory.
            '';
          };
        };

        sliceConfig = mkOption {
          type = types.attrsOf types.anything;
          default = { };
          example = {
            MemoryMax = "4G";
            CPUQuota = "200%";
          };
          description = ''
            The [Slice] section of the systemd user unit sandbox-<name>.slice.
            All programs of the sandbox run in that slice, so a limit here is
            a limit for the whole sandbox.
          '';
        };

        extraBubblewrapArgs = strList "Arguments for bwrap. The same as `backendArgs.bwrap`.";
        extraRunscArgs = strList "Arguments for runsc. The same as `backendArgs.gvisor`.";
        extraQemuArgs = strList "Arguments for a backend with the name microvm. The same as `backendArgs.microvm`.";
        extraSandboxExecArgs = strList "Arguments for a backend with the name seatbelt. The same as `backendArgs.seatbelt`.";
      };

      config = {
        backendArgs = {
          bwrap = config.extraBubblewrapArgs;
          gvisor = config.extraRunscArgs;
          microvm = config.extraQemuArgs;
          seatbelt = config.extraSandboxExecArgs;
        };

        env = lib.mkMerge [
          (lib.listToAttrs (map parseEnv config.extraEnvs))
          # The profile that the module writes to /etc/dconf/profile.
          (lib.mkIf (config.dconfSettings != { }) {
            DCONF_PROFILE = lib.mkDefault "/etc/dconf/profile/sandbox-${name}";
          })
        ];

        xdgRemap = lib.mkIf config.noXdgRemap (lib.mkForce false);
        mountCommonHomeFolders = lib.mkIf (lib.elem "common-tools" config.mounts) true;
        mountHomeCache = lib.mkIf (lib.elem "caches" config.mounts) true;

        packages = lib.mkMerge [
          (lib.mapAttrsToList (command: _: commandWrapper command) config.commands)
          # The wrapper is the last package, so it is the first in PATH.
          (lib.mkIf config.yolo (lib.mkAfter [ yoloClaude ]))
        ];
      };
    };

  # The rules of the OpenSnitch daemon of a sandbox: a directory of JSON files.
  opensnitchRules =
    name: e:
    pkgs.linkFarm "sandbox-${name}-opensnitch-rules" (
      lib.mapAttrsToList (rule: value: {
        name = "${rule}.json";
        path = pkgs.writeText "rule" (builtins.toJSON value);
      }) (cfg.opensnitch.rules // e.opensnitch.rules)
    );

  anyOpensnitch = lib.any (e: e.opensnitch.enable) (lib.attrValues cfg.environments);

  anyNix = lib.any (e: e.nix) (lib.attrValues cfg.environments);

  # The configuration of the OpenSnitch daemons of the sandboxes: the
  # configuration of the daemon of the host, with the changes for a user
  # namespace. The `sandbox` command adds the name, the rules directory and
  # the log file of each sandbox.
  opensnitchConfig =
    let
      host = config.services.opensnitch;
      settings = lib.foldl lib.recursiveUpdate { } [
        (lib.optionalAttrs host.enable (
          removeAttrs host.settings [
            "Ebpf"
            "Audit"
            "Rules"
          ]
        ))
        {
          # eBPF and audit need root on the host. In a user namespace, only
          # "proc" works.
          ProcMonitorMethod = "proc";
          # The nftables backend uses netlink, and needs no command.
          Firewall = "nftables";
          FwOptions = {
            ConfigPath = "${cfg.opensnitch.package}/etc/opensnitchd/system-fw.json";
            # If no daemon reads the queue, the kernel drops the packets: the
            # sandbox has no network while the daemon restarts.
            QueueBypass = false;
          };
        }
        cfg.opensnitch.settings
      ];
    in
    pkgs.runCommand "sandbox-opensnitch.json" { nativeBuildInputs = [ pkgs.jq ]; } ''
      jq -s '.[0] * .[1]' ${cfg.opensnitch.package}/etc/opensnitchd/default-config.json \
        ${jsonFormat.generate "settings.json" settings} >"$out"
    '';

  # OpenSSH checks the owner of each file that an Include of ssh_config
  # names: root or the user. In a sandbox the root of the host is not in the
  # user namespace, thus the files of /nix/store have the owner nobody, and
  # ssh stops ("Bad owner or permissions"). /etc/ssh/ssh_config of NixOS
  # includes a file of systemd (programs.ssh.systemd-ssh-proxy). The sandboxes
  # get a copy of /etc/ssh/ssh_config with the content of each included file
  # of /nix/store in place of its Include. An included file can start Host
  # blocks, thus the line of the Host or Match block of the Include comes
  # again after the content. ssh does not check the owner of ssh_config
  # itself.
  sshConfig =
    let
      etc = config.environment.etc."ssh/ssh_config" or null;
    in
    if etc == null || !etc.enable then
      null
    else
      pkgs.runCommand "sandbox-ssh_config" { } ''
        block="Host *"
        while IFS= read -r line || [ -n "$line" ]; do
          trimmed=''${line#"''${line%%[![:space:]]*}"}
          case ''${trimmed,,} in
            host[[:space:]]* | match[[:space:]]*) block=$trimmed ;;
          esac
          case $line in
            "Include /nix/store/"*)
              printf '# %s\n' "$line"
              for f in ''${line#Include }; do
                cat "$f"
                printf '\n'
              done
              printf '%s\n' "$block"
              ;;
            *) printf '%s\n' "$line" ;;
          esac
        done <${etc.source} >$out
      '';

  # The JSON file that the `sandbox` command reads.
  toJSON = name: e: {
    inherit (e)
      description
      color
      gtkTheme
      backend
      hostname
      persistentHome
      homeDirectory
      homePatterns
      paths
      env
      sshWrite
      gpg
      docker
      fuse
      gui
      x11
      dbus
      notifications
      nvidia
      kvm
      audio
      libvirt
      nix
      allowHome
      internetAccess
      lanAccess
      allowHosts
      disableNetworking
      shareHostNetwork
      mountHomeCache
      mountCommonHomeFolders
      mountTmp
      sharedTmp
      cleanTmp
      privacyFilter
      yolo
      backendArgs
      ;
    # The access of each program, with sshWrite in ssh.
    inherit (lib.genAttrs (lib.attrNames programConfigs) (accessOf e))
      ssh
      git
      claude
      codex
      gemini
      hermes
      opencode
      zed
      direnv
      ;
    configMounts = configMounts e;
    # The privacy filter without socksProxy: the proxy on 127.0.0.1:1080 of
    # the host, at 10.0.2.2 for slirp4netns.
    socksProxy =
      if e.socksProxy == null && e.privacyFilter then "socks5://10.0.2.2:1080" else e.socksProxy;
    allowHostLoopback = e.allowHostLoopback || (e.privacyFilter && e.socksProxy == null);
    passEnv = lib.unique (cfg.passEnv ++ e.passEnv);
    opensnitch = {
      inherit (e.opensnitch) enable directory;
      rules = if e.opensnitch.enable then "${opensnitchRules name e}" else null;
    };
    files = lib.mapAttrs (_: f: "${f.source}") e.files;
    bin = map (p: "${lib.getBin p}/bin") e.packages;
  };

  configFile = pkgs.writeText "sandboxes.json" (
    builtins.toJSON {
      menu = { inherit (cfg.menu) command markup; };
      host = { inherit (cfg.host) color; };
      backends = lib.mapAttrs (_: lib.getExe) cfg.backends;
      init = lib.getExe' cfg.initPackage "sandbox-init";
      systemd = "${config.systemd.package}";
      # The programs that run only in sandboxes: name, then sandbox.
      commands = lib.mapAttrs (
        _: uses:
        lib.listToAttrs (
          map (
            u:
            lib.nameValuePair u.env {
              program = "${lib.getBin u.c.package}/bin/${u.c.program}";
              inherit (u.c) cwdOnly bwrapArgs;
            }
          ) uses
        )
      ) commandUses;
      sshConfig = if sshConfig == null then null else "${sshConfig}";
      inherit (cfg) homeCacheDirs commonHomeFolders;
      opensnitch =
        if anyOpensnitch then
          {
            daemon = lib.getExe' cfg.opensnitch.package "opensnitchd";
            config = "${opensnitchConfig}";
          }
        else
          null;
      environments = lib.mapAttrs toJSON cfg.environments;
    }
  );
in
{
  options.programs.sandboxes = {
    enable = lib.mkEnableOption "named sandboxes for programs";

    package = mkOption {
      type = types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The `sandbox` command.";
    };

    backends = mkOption {
      type = types.attrsOf types.package;
      default = { };
      description = ''
        The backends. A backend is a command with the arguments
        `run|start|exec|show PLAN.json`. Refer to README.md for the format
        of the plan. The module adds the built-in backends bwrap and gvisor;
        a backend here is in addition to them, or replaces one of them.
      '';
    };

    initPackage = mkOption {
      type = types.package;
      default = cfg.package;
      defaultText = lib.literalExpression "config.programs.sandboxes.package";
      description = ''
        The package of `bin/sandbox-init`, the first program in a sandbox: it
        starts the private session bus and X server.
      '';
    };

    defaultBackend = mkOption {
      type = types.str;
      default = "bwrap";
      description = "The backend of a sandbox that sets no backend.";
    };

    stateDir = mkOption {
      type = types.str;
      default = "~/.local/share/sandboxes";
      description = "The directory of the home directories of the sandboxes.";
    };

    defaults = mkOption {
      type = types.deferredModule;
      default = { };
      example = lib.literalExpression ''
        { name, config, ... }: {
          homePatterns = [ ".zshrc" ];
          env.PROMPT_PREFIX = name;
        }
      '';
      description = ''
        A module that applies to each sandbox. It gets the `name` and the
        `config` of the sandbox, and it can declare options.
      '';
    };

    environments = mkOption {
      type = types.attrsOf (
        types.submoduleWith {
          modules = [
            envModule
            cfg.defaults
          ];
          shorthandOnlyDefinesConfig = true;
        }
      );
      default = { };
      description = "The sandboxes.";
    };

    passEnv = mkOption {
      type = types.listOf types.str;
      default = [
        "HOME"
        "USER"
        "LOGNAME"
        "SHELL"
        "PATH"
        "TERM"
        "COLORTERM"
        "TERMINFO"
        "TERMINFO_DIRS"
        "LANG"
        "LANGUAGE"
        "LC_ALL"
        "LC_CTYPE"
        "TZ"
        "TZDIR"
        "LOCALE_ARCHIVE"
        "LOCALE_ARCHIVE_2_27"
        "NIX_PROFILES"
        "NIX_PATH"
        "NIX_SSL_CERT_FILE"
        "SSL_CERT_FILE"
        "EDITOR"
        "VISUAL"
        "PAGER"
        "XDG_DATA_DIRS"
        "XDG_CONFIG_DIRS"
        "XDG_CURRENT_DESKTOP"
        "XDG_SESSION_DESKTOP"
        "XCURSOR_PATH"
        "XCURSOR_THEME"
        "XCURSOR_SIZE"
        "GIO_EXTRA_MODULES"
        "GTK_PATH"
        "GDK_PIXBUF_MODULE_FILE"
        "QT_PLUGIN_PATH"
        "QT_QPA_PLATFORM"
        "QT_QPA_PLATFORMTHEME"
        "QT_STYLE_OVERRIDE"
        "NIXOS_OZONE_WL"
        "MOZ_ENABLE_WAYLAND"
        "LIBVA_DRIVERS_PATH"
        "__EGL_VENDOR_LIBRARY_DIRS"
        "GST_PLUGIN_SYSTEM_PATH_1_0"
        "GST_PLUGIN_PATH_1_0"
        "GTK2_RC_FILES"
      ]
      # The variables of the NixOS session: search paths and toolkit settings.
      ++ lib.attrNames (
        config.environment.variables
        // config.environment.sessionVariables
        // config.environment.profileRelativeSessionVariables
      );
      defaultText = lib.literalMD "a list of common variables, and the names of `environment.variables`, `environment.sessionVariables` and `environment.profileRelativeSessionVariables`";
      description = ''
        Environment variables that go from the caller into each sandbox. All
        other variables stay out.
      '';
    };

    homeCacheDirs = mkOption {
      type = types.listOf types.str;
      default = [
        ".cache/go-build"
        ".cache/pip"
        ".cache/uv"
        ".cache/yarn"
        ".cache/pnpm"
        ".cache/bun"
        ".cache/zig"
        ".cache/deno"
        ".npm/_cacache"
      ];
      description = "The paths below $HOME that `mountHomeCache` mounts.";
    };

    commonHomeFolders = mkOption {
      type = types.listOf types.str;
      default = [
        ".cargo"
        ".rustup"
        ".npm"
        ".bun"
        ".deno"
        "go"
        ".gradle"
        ".m2"
        ".local/share/pnpm"
      ];
      description = "The paths below $HOME that `mountCommonHomeFolders` mounts.";
    };

    opensnitch = {
      package = mkOption {
        type = types.package;
        default = config.services.opensnitch.package;
        defaultText = lib.literalExpression "config.services.opensnitch.package";
        description = ''
          The OpenSnitch daemon of the sandboxes with `opensnitch.enable`. It
          must open its netfilter queue in a user namespace, and send its name
          (Server.Name) to the GUI.
        '';
      };

      settings = mkOption {
        type = jsonFormat.type;
        default = { };
        example = {
          DefaultAction = "deny";
          LogLevel = 1;
        };
        description = ''
          Settings of the daemons of the sandboxes. They replace the settings
          of the daemon of the host (`services.opensnitch.settings`) and the
          settings for a user namespace (ProcMonitorMethod "proc", Firewall
          "nftables", FwOptions.QueueBypass false).
        '';
      };

      rules = mkOption {
        type = types.attrsOf jsonFormat.type;
        default = lib.optionalAttrs config.services.opensnitch.enable config.services.opensnitch.rules;
        defaultText = lib.literalExpression "config.services.opensnitch.rules";
        description = ''
          The rules of all daemons of the sandboxes. The default is the rules
          of the daemon of the host, so that a rule of the configuration
          applies to all daemons. Programs in a sandbox have the same paths
          in /nix/store as on the host, so the rules match the same programs.
        '';
      };
    };

    notifications = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Send a notification (notify-send, on the session bus of the host)
        when the container of a sandbox starts and when it stops. A
        container that stops with an error, or that a signal or the OOM
        killer kills, gives a critical notification with the reason.
      '';
    };

    host.color = mkOption {
      type = types.strMatching "#[0-9a-fA-F]{6}";
      default = "#404040";
      description = ''
        The label color of the host. A window manager uses it for the
        windows of all processes in no sandbox slice. `sandbox host -- CMD`
        runs CMD outside of all sandboxes with SANDBOX_NAME=host and this
        color in SANDBOX_COLOR (for a shell prompt), and the menu of
        `sandbox pick --host` shows it.
      '';
    };

    menu = {
      command = mkOption {
        type = types.str;
        default = "${lib.getExe pkgs.rofi} -dmenu -i -p sandbox";
        defaultText = lib.literalExpression ''"''${lib.getExe pkgs.rofi} -dmenu -i -p sandbox"'';
        description = ''
          The menu of `sandbox pick`. It reads one line for each sandbox and
          writes the selected line. The first word of the line is the name.
        '';
      };
      markup = mkOption {
        type = types.bool;
        default = false;
        description = ''
          Whether the lines of the menu have Pango markup with the color of
          each sandbox. Use it with `rofi -markup-rows`.
        '';
      };
    };

    configFile = mkOption {
      type = types.path;
      readOnly = true;
      default = configFile;
      description = "The JSON file of the sandboxes, in /etc/sandboxes/config.json.";
    };
  };

  config = lib.mkIf cfg.enable {
    # The built-in backends. A definition of the configuration adds to them
    # (the default of an option would be replaced).
    programs.sandboxes.backends = {
      bwrap = lib.mkDefault backendPackages.bwrap;
      gvisor = lib.mkDefault backendPackages.gvisor;
    };

    assertions = [
      {
        assertion = config.security.allowUserNamespaces;
        message = "programs.sandboxes needs security.allowUserNamespaces.";
      }
    ]
    ++ lib.concatLists (
      lib.mapAttrsToList (name: e: [
        {
          # A dash in a slice name makes a child slice: sandbox-a-b.slice is
          # below sandbox-a.slice.
          assertion = builtins.match "[a-z0-9_]+" name != null;
          message = "programs.sandboxes.environments.${name}: use only a-z, 0-9 and _ in the name.";
        }
        {
          assertion =
            !(
              e.shareHostNetwork && (e.socksProxy != null || e.privacyFilter || !e.internetAccess || !e.lanAccess)
            );
          message = "programs.sandboxes.environments.${name}: shareHostNetwork has no firewall and no proxy.";
        }
        {
          assertion = e.privacyFilter -> !e.disableNetworking;
          message = "programs.sandboxes.environments.${name}: privacyFilter needs a network (not disableNetworking).";
        }
        {
          assertion = name != "host";
          message = "programs.sandboxes.environments.host: the name host is for the host.";
        }
        {
          assertion = e.sshWrite -> e.ssh != "none";
          message = "programs.sandboxes.environments.${name}: sshWrite needs ssh.";
        }
        {
          assertion = e.opensnitch.enable -> !(e.disableNetworking || e.shareHostNetwork);
          message = "programs.sandboxes.environments.${name}: opensnitch needs a private network (not disableNetworking or shareHostNetwork).";
        }
      ]) cfg.environments
    );

    environment.systemPackages = [
      cfg.package
    ]
    ++ lib.attrValues cfg.backends
    ++ lib.mapAttrsToList (name: _: commandWrapper name) commandUses;

    # A systemd user slice for each sandbox. `sandbox run` starts each run in a
    # scope below sandbox-<name>.slice. The name in the cgroup of a process
    # tells the sandbox of the process, and a window manager reads it. A
    # process in no sandbox slice is on the host. sandbox.slice holds all of
    # them, because a dash in a slice name makes a child slice.
    systemd.user.slices = {
      sandbox.description = "Sandboxes (programs.sandboxes)";
    }
    // lib.mapAttrs' (
      name: e:
      lib.nameValuePair "sandbox-${name}" {
        description = "Sandbox ${name}" + lib.optionalString (e.description != "") ": ${e.description}";
        sliceConfig = e.sliceConfig;
      }
    ) cfg.environments;

    # A dconf profile for each sandbox with dconfSettings: the database of the
    # sandbox home directory, then the settings of the configuration.
    programs.dconf.profiles = lib.mapAttrs' (
      name: e:
      lib.nameValuePair "sandbox-${name}" {
        enableUserDb = true;
        databases = [
          {
            settings = lib.mapAttrs (
              _: lib.mapAttrs (_: v: if builtins.isInt v then lib.gvariant.mkInt32 v else v)
            ) e.dconfSettings;
          }
        ];
      }
    ) (lib.filterAttrs (_: e: e.dconfSettings != { }) cfg.environments);
    environment.etc."sandboxes/config.json".source = configFile;

    # A program in a sandbox that has the terminal of the caller cannot put
    # characters into the input of that terminal (TIOCSTI).
    boot.kernel.sysctl."dev.tty.legacy_tiocsti" = lib.mkDefault 0;

    # The container of a sandbox: the network namespace with slirp4netns, the
    # firewall and the OpenSnitch daemon, and the backend with the private X
    # server and session bus. Each run in a scope starts its program in the
    # container. Each run requires the service, so the service starts before
    # the first run and stops after the last run. The service is in the slice
    # of the sandbox: a window manager finds the sandbox of the X server from
    # its cgroup.
    systemd.user.services."sandbox@" = {
      description = "Container of the sandbox %i";
      unitConfig.StopWhenUnneeded = true;
      serviceConfig = {
        # The service tells systemd when the container can start programs,
        # and systemd starts the scope of a run only then (After=).
        Type = "notify";
        NotifyAccess = "main";
        ExecStart = "${lib.getExe cfg.package} __container %i";
        Slice = "sandbox-%i.slice";
      }
      # ExecStartPost runs when the container is ready. ExecStopPost runs
      # after each stop, also after a kill. "-": an error of the
      # notification does not make the service fail.
      // lib.optionalAttrs cfg.notifications {
        ExecStartPost = "-${lib.getExe cfg.package} __notify started %i";
        ExecStopPost = "-${lib.getExe cfg.package} __notify stopped %i";
      };
    };

    # The Nix daemon for the sandboxes with `nix`: a socket in the runtime
    # directory of the user, mounted at /nix/var/nix/daemon-socket. For each
    # connection, nix-daemon runs as the user, does not trust the client,
    # and sends the operations to the Nix daemon of the host.
    systemd.user.sockets.sandbox-nix-daemon = lib.mkIf anyNix {
      description = "Nix daemon for the sandboxes (untrusted)";
      wantedBy = [ "sockets.target" ];
      socketConfig = {
        ListenStream = "%t/sandbox-nix-daemon/socket";
        Accept = true;
        SocketMode = "0600";
        DirectoryMode = "0700";
      };
    };
    systemd.user.services."sandbox-nix-daemon@" = lib.mkIf anyNix {
      description = "Nix daemon connection of a sandbox (untrusted)";
      environment.NIX_REMOTE = "daemon";
      serviceConfig = {
        # --force-untrusted needs the experimental feature daemon-trust-override.
        # Also --process-ops is necessary (it needs mounted-ssh-store): without
        # it, nix-daemon --stdio with a daemon store does not process the
        # operations, also with --force-untrusted. It then forwards the bytes
        # with splice(), which fails from a socket to a socket (EINVAL), thus
        # each connection stops. If the forward worked, the daemon of the
        # host would trust the client, because the user is in trusted-users.
        ExecStart = "${lib.getExe' config.nix.package "nix-daemon"} --extra-experimental-features 'daemon-trust-override mounted-ssh-store' --stdio --force-untrusted --process-ops";
        StandardInput = "socket";
        StandardOutput = "socket";
        # The socket is only for the protocol of Nix.
        StandardError = "journal";
      };
    };

    services.opensnitch.rules = lib.mkIf config.services.opensnitch.enable (
      lib.mapAttrs' (
        name: _:
        lib.nameValuePair "sandbox-${name}-slirp4netns" {
          name = "sandbox-${name}-slirp4netns";
          enabled = true;
          action = "allow";
          duration = "always";
          created = "1970-01-01T00:00:00Z";
          operator = {
            type = "list";
            operand = "list";
            list = [
              {
                type = "simple";
                operand = "process.path";
                sensitive = false;
                data = lib.getExe pkgs.slirp4netns;
              }
              {
                type = "simple";
                operand = "process.env.SANDBOX_NAME";
                sensitive = true;
                data = name;
              }
            ];
          };
        }
      ) (lib.filterAttrs (_: e: e.opensnitch.enable && e.opensnitch.allowOnHost) cfg.environments)
    );
  };
}
