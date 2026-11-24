# A NixOS VM test of the network modes, the home directory and the label
# environment. The node "server" has a local address (192.168.1.2) and an
# address that is not local (203.0.113.10, "the internet").
{
  module,
  # The OpenSnitch fork (overlays.opensnitch of flake.nix), for the sandbox
  # with opensnitch.
  opensnitchOverlay ? null,
}:
{ pkgs, ... }:
let
  # A backend of the configuration (the interface of README.md): it writes
  # its mode to /tmp/logged-backend.log and gives the plan to bwrap.
  loggedBackend = pkgs.writeShellScriptBin "logged-backend" ''
    echo "$1" >> /tmp/logged-backend.log
    exec sandbox-backend-bwrap "$@"
  '';
  # A command that prints the sandbox, the directory and the files in ~/work
  # that it sees.
  whereami = pkgs.writeShellScriptBin "whereami" ''
    echo "$SANDBOX_NAME:$PWD:$(ls ~/work 2>/dev/null | xargs)"
  '';
  # The same for a cwdOnly command, with the host name and ~/notes. It also
  # writes a file in its current directory.
  whereamiCwd = pkgs.writeShellScriptBin "whereami-cwd" ''
    touch written
    echo "$SANDBOX_NAME:$PWD:$(ls ~/work | xargs):$(hostname):$(cat ~/notes 2>/dev/null)"
  '';
  # A notification server on the session bus of the host: it writes each
  # Notify as one line of JSON to the file of its argument.
  fakeNotifications = pkgs.writers.writePython3Bin "fake-notifications" {
    libraries = [ pkgs.python3Packages.dbus-next ];
    # The annotations of dbus-next are D-Bus types, not Python names.
    doCheck = false;
  } ''
    import asyncio
    import json
    import sys

    from dbus_next.aio import MessageBus
    from dbus_next.service import ServiceInterface, method


    class Notifications(ServiceInterface):
        def __init__(self):
            super().__init__("org.freedesktop.Notifications")
            self.last = 40

        @method()
        def Notify(self, app: "s", replaces: "u", icon: "s", summary: "s",
                   body: "s", actions: "as", hints: "a{sv}",
                   timeout: "i") -> "u":
            self.last += 1
            h = {k: v.value if isinstance(v.value, (str, int)) else "?"
                 for k, v in hints.items()}
            with open(sys.argv[1], "a") as f:
                f.write(json.dumps({"app": app, "replaces": replaces,
                                    "icon": icon, "summary": summary,
                                    "body": body, "hints": h}) + "\n")
            return self.last

        @method()
        def CloseNotification(self, id: "u"):
            pass


    async def main():
        bus = await MessageBus().connect()
        bus.export("/org/freedesktop/Notifications", Notifications())
        await bus.request_name("org.freedesktop.Notifications")
        await asyncio.get_running_loop().create_future()


    asyncio.run(main())
  '';
  # The settings portal of the host: color-scheme is the number in the file
  # of its argument. After a change of the file, it sends SettingChanged.
  fakePortal = pkgs.writers.writePython3Bin "fake-portal" {
    libraries = [ pkgs.python3Packages.dbus-next ];
    doCheck = false;
  } ''
    import asyncio
    import sys

    from dbus_next import Variant
    from dbus_next.aio import MessageBus
    from dbus_next.service import ServiceInterface, method, signal


    def scheme():
        with open(sys.argv[1]) as f:
            return int(f.read())


    class Settings(ServiceInterface):
        def __init__(self):
            super().__init__("org.freedesktop.portal.Settings")

        @method()
        def ReadOne(self, namespace: "s", key: "s") -> "v":
            return Variant("u", scheme())

        @signal()
        def SettingChanged(self) -> "ssv":
            return ["org.freedesktop.appearance", "color-scheme",
                    Variant("u", scheme())]


    async def main():
        bus = await MessageBus().connect()
        settings = Settings()
        bus.export("/org/freedesktop/portal/desktop", settings)
        await bus.request_name("org.freedesktop.portal.Desktop")
        last = scheme()
        while True:
            await asyncio.sleep(0.2)
            if scheme() != last:
                last = scheme()
                settings.SettingChanged()


    asyncio.run(main())
  '';
in
{
  name = "sandboxes";
  node.pkgsReadOnly = false;

  nodes.server = {
    networking.firewall.allowedTCPPorts = [
      80
      1080
    ];
    networking.interfaces.eth1.ipv4.addresses = [
      {
        address = "203.0.113.10";
        prefixLength = 24;
      }
    ];
    services.nginx = {
      enable = true;
      virtualHosts.default.locations."/".return = "200 ok";
    };
    # A SOCKS proxy for the sandbox with socksProxy.
    systemd.services.microsocks = {
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      serviceConfig.ExecStart = "${pkgs.microsocks}/bin/microsocks -i 192.168.1.2 -p 1080";
    };
  };

  nodes.client = {
    imports = [ module ];
    networking.interfaces.eth1.ipv4.routes = [
      {
        address = "203.0.113.0";
        prefixLength = 24;
      }
    ];
    environment.systemPackages = [
      fakeNotifications
      fakePortal
      pkgs.dconf
      pkgs.curl
      pkgs.libnotify
      pkgs.microsocks
      pkgs.openssh
      pkgs.procps
      pkgs.socat
    ];
    nixpkgs.overlays = pkgs.lib.optional (opensnitchOverlay != null) opensnitchOverlay;
    # The daemon of a sandbox makes its nftables rules in a user namespace,
    # where the kernel does not load modules.
    boot.kernelModules = [
      "nf_tables"
      "nft_ct"
      "nft_queue"
      "nfnetlink_queue"
    ];
    # The dconf service of the private buses (followColorScheme).
    programs.dconf.enable = true;
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };
    # A trusted user: the sandbox with `nix` must be untrusted all the same.
    nix.settings.trusted-users = [ "alice" ];
    programs.sandboxes = {
      enable = true;
      backends.logged = loggedBackend;
      # The test selects the line of the menu with a variable.
      menu.command = "${pkgs.gnugrep}/bin/grep -m1 \"$SANDBOX_TEST_PICK\"";
      opensnitch.settings.DefaultAction = "allow";
      environments = {
        ext.backend = "logged";
        socks.socksProxy = "192.168.1.2:1080";
        privacy.privacyFilter = true;
        osn.opensnitch = {
          enable = true;
          rules.deny-inet = {
            name = "deny-inet";
            enabled = true;
            action = "deny";
            duration = "always";
            created = "1970-01-01T00:00:00Z";
            operator = {
              type = "simple";
              operand = "dest.ip";
              sensitive = false;
              data = "203.0.113.10";
            };
          };
        };
        net = {
          color = "#008080";
          commands = { inherit whereami; };
          followColorScheme = true;
        };
        lanonly.internetAccess = false;
        inetonly.lanAccess = false;
        allowlisted = {
          internetAccess = false;
          allowHosts = [ "203.0.113.10" ];
        };
        none.disableNetworking = true;
        quiet.notifications = false;
        hostnet.shareHostNetwork = true;
        disposable.persistentHome = false;
        sharedtmp.sharedTmp = true;
        # The store of the host, without a network.
        withnix = {
          nix = true;
          disableNetworking = true;
        };
        # The access options of sandboxed-agents.
        agents = {
          git = true;
          claude = "write";
          zed = "read";
          direnv = "read";
          extraEnvs = [ "FOO=a=b" ];
          yolo = true;
          # A directory and a file.
          paths = [
            "~/work"
            "~/notes"
          ];
          # Programs that run only in sandboxes. whereami is also in net.
          commands = {
            inherit whereami;
            whereami-cwd = {
              package = whereamiCwd;
              cwdOnly = true;
              bwrapArgs = [
                "--unshare-uts"
                "--hostname"
                "nested"
              ];
            };
          };
        };
        # A path with a "/" at the end.
        slashed = {
          paths = [ "~/work/" ];
          commands.whereami-cwd = {
            package = whereamiCwd;
            cwdOnly = true;
          };
        };
        # A path that has the configuration of Claude Code: cwdOnly would
        # hide it.
        homeagent = {
          paths = [ "~" ];
          claude = "write";
          commands.whereami-cwd = {
            package = whereamiCwd;
            cwdOnly = true;
          };
        };
        # The same checks with gVisor.
        gnet = {
          backend = "gvisor";
          color = "#800080";
          followColorScheme = true;
        };
        glanonly = {
          backend = "gvisor";
          internetAccess = false;
        };
        gnone = {
          backend = "gvisor";
          disableNetworking = true;
          gui = false;
        };
        ghostnet = {
          backend = "gvisor";
          shareHostNetwork = true;
        };
      };
    };
  };

  testScript = ''
    start_all()
    server.wait_for_unit("nginx.service")
    client.wait_for_unit("multi-user.target")
    client.wait_for_unit("user@1000.service")

    def sb(env, cmd, ok=True):
        line = f"su - alice -c 'SANDBOX_NO_SCOPE=1 sandbox run {env} -- sh -c \"{cmd}\"'"
        return client.succeed(line) if ok else client.fail(line)

    with subtest("plans"):
        # PATH of a sandbox: the bin directories of the packages, the last
        # first, and then the PATH of the caller (not the PATH of the
        # sandbox command).
        import json
        conf = json.loads(client.succeed("cat /etc/sandboxes/config.json"))
        alice_path = client.succeed("su - alice -c 'echo $PATH'").strip()
        for env in sorted(conf["environments"]):
            plan = json.loads(client.succeed(f"su - alice -c 'sandbox __plan run {env} /T /S \"\" -- true'"))
            want = ":".join(list(reversed(conf["environments"][env]["bin"])) + [alice_path])
            assert plan["env"]["PATH"] == want, f"{env}: {plan['env']['PATH']} != {want}"
        # A program with the terminal of the caller cannot put characters
        # into its input.
        client.succeed("test $(cat /proc/sys/dev/tty/legacy_tiocsti) = 0")

    lan = "curl -sf -m 5 http://192.168.1.2/"
    inet = "curl -sf -m 5 http://203.0.113.10/"

    with subtest("network modes"):
        for env, lan_ok, inet_ok in [
            ("net", True, True),
            ("lanonly", True, False),
            ("inetonly", False, True),
            ("allowlisted", True, True),
            ("none", False, False),
            ("hostnet", True, True),
            ("gnet", True, True),
            ("glanonly", True, False),
            ("gnone", False, False),
            ("ghostnet", True, True),
        ]:
            sb(env, lan, lan_ok)
            sb(env, inet, inet_ok)

    with subtest("loopback of the host"):
        # A service on 127.0.0.1 of the host, as a Gradle daemon. After the
        # end of its input, socat waits -t seconds for the answer: gVisor
        # can need more than the default 0.5 s.
        client.succeed("su - alice -c 'socat TCP-LISTEN:9000,bind=127.0.0.1,fork,reuseaddr SYSTEM:\"echo hi\" >/dev/null 2>&1 &'")
        client.wait_for_open_port(9000)
        for env, ok in [("hostnet", True), ("ghostnet", True), ("net", False), ("gnet", False)]:
            sb(env, "socat -T 3 -t 3 - TCP:127.0.0.1:9000 </dev/null | grep -q hi", ok)

    with subtest("ping"):
        for env, host, ok in [
            ("net", "192.168.1.2", True),
            ("net", "203.0.113.10", True),
            ("lanonly", "203.0.113.10", False),
            ("inetonly", "192.168.1.2", False),
            ("hostnet", "192.168.1.2", True),
            ("gnet", "203.0.113.10", True),
        ]:
            sb(env, f"ping -c 1 -W 3 {host}", ok)

    with subtest("home directory"):
        client.succeed("su - alice -c 'echo secret > ~/secret'")
        sb("net", "test ! -e ~/secret && echo kept > ~/kept")
        client.succeed("test -e /home/alice/.local/share/sandboxes/net/home/kept")
        sb("disposable", "echo x > ~/gone")
        sb("disposable", "test ! -e ~/gone")

    with subtest("shared /tmp"):
        # All runs of the sandbox get the same /tmp. Other sandboxes do not.
        sb("sharedtmp", "echo x > /tmp/shared")
        sb("sharedtmp", "test -e /tmp/shared")
        client.succeed("test $(stat -c %a:%U /tmp/sandbox-1000-sharedtmp) = 700:alice")
        sb("net", "echo y > /tmp/private")
        sb("net", "test ! -e /tmp/private && test ! -e /tmp/shared")
        # A directory of a different user is an error.
        client.succeed("rm -rf /tmp/sandbox-1000-sharedtmp && mkdir -m 0777 /tmp/sandbox-1000-sharedtmp")
        sb("sharedtmp", "true", ok=False)
        client.succeed("rmdir /tmp/sandbox-1000-sharedtmp")
        sb("sharedtmp", "true")

    with subtest("configuration of programs"):
        client.succeed("su - alice -c 'mkdir -p ~/.claude ~/.config/zed ~/.local/share/zed ~/.local/share/direnv/allow && echo [user] > ~/.gitconfig && echo {} > ~/.config/zed/settings.json'")
        # "read": the files are there, and the sandbox cannot change them.
        sb("agents", "test -r ~/.gitconfig && ! touch ~/.gitconfig")
        sb("agents", "test -r ~/.config/zed/settings.json && ! touch ~/.config/zed/new")
        sb("agents", "test -d ~/.local/share/zed && ! touch ~/.local/share/zed/new")
        sb("agents", "test -d ~/.local/share/direnv/allow && ! touch ~/.local/share/direnv/allow/new")
        # "write": the files go to the host.
        sb("agents", "echo x > ~/.claude/new")
        client.succeed("test -e /home/alice/.claude/new")
        # A sandbox without the options does not get the files.
        sb("net", "test ! -e ~/.gitconfig && test ! -e ~/.claude/new")
        assert sb("agents", "echo \\$FOO").strip() == "a=b"
        # yolo: the claude first in PATH starts the next claude with
        # --dangerously-skip-permissions.
        bin = "/home/alice/.local/share/sandboxes/agents/home/bin"
        client.succeed(f"mkdir -p {bin} && cat > {bin}/claude <<'EOF'\n#!/bin/sh\necho \"claude $@\"\nEOF\nchmod +x {bin}/claude && chown -R alice {bin}")
        out = sb("agents", "PATH=\\$PATH:/home/alice/bin claude x").strip()
        assert out == "claude --dangerously-skip-permissions x", out

    with subtest("commands"):
        client.succeed("su - alice -c 'mkdir -p ~/work/a ~/work/b && echo secret-notes > ~/notes'")
        # On the host and in the sandboxes without the command, it stops.
        out = client.fail("su - alice -c 'cd ~/work && whereami 2>&1'")
        assert "runs only in the sandboxes agents, net, not on the host" in out, out
        out = client.fail("su - alice -c 'cd ~/work && SANDBOX_NAME=agents whereami 2>&1'")
        assert "not on the host" in out, out
        assert client.succeed("su - alice -c 'SANDBOX_NAME=net sandbox current'").strip() == "host"
        out = sb("lanonly", "whereami 2>&1", ok=False)
        assert "not in the sandbox lanonly" in out, out
        out = sb("lanonly", "whereami-cwd 2>&1", ok=False)
        assert "runs only in the sandboxes agents, homeagent, slashed, not in the sandbox lanonly" in out, out
        assert sb("net", "sandbox current").strip() == "net"
        # In each sandbox with the command, it starts the program.
        assert sb("net", "whereami").strip().startswith("net:/home/alice")
        # In its sandbox, the command starts the program, first in PATH.
        assert sb("agents", "cd ~/work/a && whereami").strip() == "agents:/home/alice/work/a:a b"
        assert sb("agents", "command -v whereami").strip().startswith("/nix/store/")
        # cwdOnly: of the paths ~/work and ~/notes, the program sees only the
        # current directory. The nested bwrap gets the arguments.
        assert sb("agents", "cat ~/notes").strip() == "secret-notes"
        assert sb("agents", "cd ~/work/a && whereami-cwd").strip() == "agents:/home/alice/work/a:a:nested:"
        assert sb("agents", "cd ~/work && whereami-cwd").strip() == "agents:/home/alice/work:a b written:nested:"
        assert sb("agents", "cd ~ && whereami-cwd").strip() == "agents:/home/alice::nested:"
        # The program can change the files of the current directory.
        client.succeed("test -e /home/alice/work/a/written")
        # sandbox run --need-cwd: the sandbox must have the current directory.
        client.succeed("su - alice -c 'cd ~/work && SANDBOX_NO_SCOPE=1 sandbox run --need-cwd agents -- true'")
        out = client.fail("su - alice -c 'cd ~ && SANDBOX_NO_SCOPE=1 sandbox run --need-cwd agents -- true 2>&1'")
        assert "does not have the current directory" in out, out
        # A path with a "/" at the end has the current directory too.
        out = client.succeed("su - alice -c 'cd ~/work && SANDBOX_NO_SCOPE=1 sandbox run --need-cwd slashed -- pwd'")
        assert out.strip() == "/home/alice/work", out
        assert sb("slashed", "cd ~/work/a && whereami-cwd").strip() == "slashed:/home/alice/work/a:a:slashed:"
        # A $PWD with an empty segment names the same directory. With a ".."
        # segment, the test takes the directory without links: ~/x is not
        # below ~/work.
        out = client.succeed("su - alice -c 'cd ~/work/a && PWD=/home/alice//work/a SANDBOX_NO_SCOPE=1 sandbox run --need-cwd agents -- realpath .'")
        assert out.strip() == "/home/alice/work/a", out
        out = client.fail("su - alice -c 'mkdir -p ~/x && cd ~/x && PWD=/home/alice/work/../x SANDBOX_NO_SCOPE=1 sandbox run --need-cwd agents -- true 2>&1'")
        assert "does not have the current directory /home/alice/x" in out, out
        out = sb("agents", "cd ~/work/a && PWD=/home/alice//work/a whereami-cwd").strip()
        assert out.endswith(":a:nested:"), out
        # cwdOnly does not start when a path has the configuration of a program.
        out = sb("homeagent", "cd ~/work/a && whereami-cwd 2>&1", ok=False)
        assert "has the configuration /home/alice/.claude" in out, out

    with subtest("nix daemon"):
        # The Nix daemon trusts alice on the host, but not in the sandbox.
        info = "nix --extra-experimental-features nix-command store info 2>&1"
        assert "Trusted: 1" in client.succeed(f"su - alice -c '{info}'")
        out = sb("withnix", info)
        assert "Trusted: 0" in out, out
        # A path that the sandbox adds is in the store of the host.
        path = sb("withnix", "echo hi > f && nix-store --add f").strip()
        assert client.succeed(f"cat {path}").strip() == "hi", path
        # A sandbox without the option has no socket.
        sb("net", "test ! -e /nix/var/nix/daemon-socket")

    with subtest("no nested sandboxes"):
        # A sandbox cannot start a sandbox: not another one, not itself, and
        # not without its label. host would give the label of the host.
        for cmd in ["sandbox run agents -- true", "sandbox run net -- true", "env -u SANDBOX_NAME sandbox run agents -- true", "SANDBOX_NAME=host sandbox host -- true", "sandbox stop agents"]:
            out = sb("net", f"{cmd} 2>&1", ok=False)
            assert "nested sandboxes are forbidden" in out, f"{cmd}: {out}"
        # No state of a nested sandbox in the home directory of the sandbox.
        sb("net", "test ! -e ~/.local/share/sandboxes")
        # The commands that start no sandbox work, and a nested bwrap too.
        assert "agents" in sb("net", "sandbox list --names")
        assert sb("net", "${pkgs.bubblewrap}/bin/bwrap --dev-bind / / -- echo nested").strip() == "nested"

    with subtest("ssh"):
        # /etc/ssh/ssh_config of NixOS includes a file of /nix/store
        # (systemd-ssh-proxy). ssh refuses an included file that is not owned
        # by root or by the user, and in a sandbox the files of the host
        # root have the owner nobody.
        client.succeed("grep -q '^Include /nix/store/' /etc/ssh/ssh_config")
        # sb() quotes the command: no quotes or spaces in the pattern.
        known = "grep -qx globalknownhostsfile./etc/ssh/ssh_known_hosts"
        client.succeed(f"su - alice -c 'ssh -G example.com | {known}'")
        for env in ["net", "gnet"]:
            # The options of the included file, and the global options after
            # the Include, stay as on the host.
            assert "proxycommand" in sb(env, "ssh -G unix/run/x"), env
            assert "proxycommand" not in sb(env, "ssh -G example.com"), env
            sb(env, f"ssh -G example.com | {known}")

    with subtest("terminal"):
        # The program gets a terminal, and the exit code of the program.
        for env in ["net", "gnet"]:
            assert "is-tty" in client.succeed(f"su - alice -c 'SANDBOX_NO_SCOPE=1 timeout 60 script -qec \"sandbox run {env} -- sh -c \\\"test -t 0 && test -t 1 && echo is-tty\\\"\" /dev/null'"), env
            rc = client.execute(f"su - alice -c 'SANDBOX_NO_SCOPE=1 timeout 60 script -qec \"sandbox run {env} -- sh -c \\\"exit 7\\\"\" /dev/null'")[0]
            assert rc == 7, f"{env}: exit code {rc}"

    with subtest("identity"):
        # The program is root in gVisor, and the user on the host.
        for env, uid in [("net", "1000"), ("gnet", "0"), ("gnone", "0")]:
            assert sb(env, "id -u").strip() == uid, env
            assert sb(env, "hostname").strip() == env, env
            sb(env, f"test ! -e ~/secret && echo {env} > ~/kept")
            client.succeed(f"test $(stat -c %u /home/alice/.local/share/sandboxes/{env}/home/kept) = 1000")
        # gVisor has its own kernel, and its log says so.
        assert "gVisor" in sb("gnet", "dmesg"), "gnet does not run in gVisor"

    with subtest("private session bus"):
        names = "dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.ListNames"
        for env in ["net", "gnet"]:
            sb(env, names)
            # Each run has its own bus, in the runtime directory of the run.
            assert sb(env, "echo \\$DBUS_SESSION_BUS_ADDRESS").strip() == "unix:path=/run/user/1000/bus", env

    with subtest("the window manager can read the label"):
        client.succeed("su - alice -c 'SANDBOX_NO_SCOPE=1 sandbox run net -- sleep 300 >/dev/null 2>&1 &'")
        pid = client.wait_until_succeeds("pgrep -u alice -x sleep").split()[0]
        env = client.succeed(f"su - alice -c 'tr \\\\0 \\\\n < /proc/{pid}/environ'")
        assert "SANDBOX_NAME=net" in env, env
        assert "SANDBOX_COLOR=#008080" in env, env
        client.succeed("pkill -u alice -x sleep")

    run = "su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "

    def sc(env, cmd, ok=True):
        # A run in a scope: the program starts in the container of the sandbox.
        line = run + f"sandbox run {env} -- sh -c \"{cmd}\"'"
        return client.succeed(line) if ok else client.fail(line)

    def active(env):
        return client.execute(run + f"systemctl --user -q is-active sandbox@{env}.service'")[0] == 0

    with subtest("container"):
        bus_id = "dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetId | tail -1"
        for env, uid in [("net", "1000"), ("none", "1000"), ("hostnet", "1000"), ("gnet", "0")]:
            # The first run starts the container; it stays while a run runs.
            client.succeed(run + f"sandbox run {env} -- sleep 300 >/dev/null 2>&1 &'")
            client.wait_until_succeeds(run + f"systemctl --user -q is-active sandbox@{env}.service'")
            # The next runs see the processes, /tmp and the session bus of the
            # container. A run without a scope has a container of its own.
            # (The container holds a "sleep 3600" of sandbox-init.) The first
            # run can start its program after the next run.
            client.wait_until_succeeds(run + f"sandbox run {env} -- sh -c \"pgrep -xf sleep.300\"'")
            sb(env, "pgrep -xf sleep.300", ok=False)
            sc(env, "echo x > /tmp/joined")
            sc(env, "test -e /tmp/joined")
            sb(env, "test ! -e /tmp/joined")
            assert sc(env, bus_id) == sc(env, bus_id), env
            assert sc(env, bus_id) != sb(env, bus_id), env
            assert sc(env, "id -u").strip() == uid, env
            assert sc(env, "hostname").strip() == env, env
            sc(env, "test ! -e ~/secret")
            # The network of the container.
            if env == "none":
                sc(env, inet, ok=False)
            else:
                sc(env, inet)
            # The exit code and the directory of the program.
            rc = client.execute(run + f"sandbox run {env} -- sh -c \"exit 7\"'")[0]
            assert rc == 7, f"{env}: exit code {rc}"
            assert sc(env, "pwd").strip() == "/home/alice", env
            # The program has no capabilities and cannot get new privileges,
            # as in a run of its own. (The /proc of gVisor has no SigIgn.)
            if env != "gnet":
                # The program gets Ctrl-C and SIGPIPE: SIGINT (bit 2) and
                # SIGPIPE (bit 13) are not ignored.
                for f in [sc, sb]:
                    ign = int(f(env, "grep SigIgn /proc/self/status").split()[1], 16)
                    assert ign & 0x1002 == 0, f"{env}: SigIgn {ign:x}"
                assert sc(env, "grep CapEff /proc/self/status").split()[1] == "0000000000000000", env
                assert sc(env, "grep NoNewPrivs /proc/self/status").split()[1] == "1", env
                # The mounts of the container stay read-only.
                sc(env, "touch /etc/x", ok=False)
            # The container stops after the last run.
            sc(env, "pkill -xf sleep.300")
            client.wait_until_succeeds(f"! su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user -q is-active sandbox@{env}.service'")
        # The program starts in the current directory if the sandbox has it.
        out = client.succeed("su - alice -c 'cd ~/work/a && XDG_RUNTIME_DIR=/run/user/1000 sandbox run --need-cwd agents -- pwd'")
        assert out.strip() == "/home/alice/work/a", out
        # A program of a run can stay after the command of the run. The scope
        # of the run, and thus the container, stay with it.
        sc("net", "sleep 302 >/dev/null 2>&1 & echo started")
        client.succeed("pgrep -u alice -xf 'sleep 302'")
        assert active("net")
        client.succeed(run + "sandbox stop net'")
        client.wait_until_fails("pgrep -u alice -xf 'sleep 302'")
        assert not active("net")
        # A configuration that gives less access than the running container
        # has: the run stops. More access: the run only warns. `sandbox ps`
        # shows the container, and that its configuration changed.
        client.succeed("su - alice -c 'mkdir -p ~/work/a && touch ~/notes'")
        jq = "${pkgs.jq}/bin/jq"
        client.succeed(f"{jq} '.environments.agents.paths = [\"~/work\"]' /etc/sandboxes/config.json > /tmp/less.json")
        client.succeed("mkdir -p /tmp/extra")
        client.succeed(f"{jq} '.environments.agents.paths += [\"/tmp/extra\"]' /etc/sandboxes/config.json > /tmp/more.json")
        client.succeed("chmod 644 /tmp/less.json /tmp/more.json")
        client.succeed(run + "sandbox run agents -- sleep 303 >/dev/null 2>&1 &'")
        client.wait_until_succeeds(run + "systemctl --user -q is-active sandbox@agents.service'")
        out = client.fail(run + "SANDBOX_CONFIG=/tmp/less.json sandbox run agents -- true 2>&1'")
        assert "has more access than its configuration gives now (/home/alice/notes)" in out, out
        out = client.succeed(run + "SANDBOX_CONFIG=/tmp/more.json sandbox run agents -- true 2>&1'")
        assert "started with different mounts (/tmp/extra)" in out, out
        out = client.succeed(run + "sandbox ps'")
        assert "[container]" in out and "stale" not in out, out
        out = client.succeed(run + "SANDBOX_CONFIG=/tmp/less.json sandbox ps'")
        assert "agents" in out and "stale" in out, out
        client.succeed(run + "sandbox stop agents'")
        assert not active("agents")
        # A terminal, and the exit code of a program with a terminal.
        import re
        for env in ["net", "gnet"]:
            # The program has a terminal of its own: it cannot change the
            # terminal of the caller.
            for prefix in [run, "su - alice -c 'SANDBOX_NO_SCOPE=1 "]:
                out = client.succeed(prefix + f"timeout 60 script -qec \"sandbox run {env} -- stty -echo; stty -a\" /dev/null'")
                assert re.search(r"(^|\s)echo(\s|$)", out) and not re.search(r"(^|\s)-echo(\s|$)", out), f"{env}: {out}"
            assert "is-tty" in client.succeed(run + f"timeout 60 script -qec \"sandbox run {env} -- sh -c \\\"test -t 0 && test -t 1 && echo is-tty\\\"\" /dev/null'"), env
            rc = client.execute(run + f"timeout 60 script -qec \"sandbox run {env} -- sh -c \\\"exit 7\\\"\" /dev/null'")[0]
            assert rc == 7, f"{env}: exit code {rc}"

    with subtest("notifications"):
        notif_log = "/tmp/notifications.log"
        client.succeed(f"su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus fake-notifications {notif_log} >/tmp/fake-notifications.out 2>&1 &'")
        has_owner = "dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner string:org.freedesktop.Notifications | grep -q true"
        client.wait_until_succeeds(run + has_owner + "'")
        for env, color in [("net", "#008080"), ("gnet", "#800080")]:
            client.succeed(f"rm -f {notif_log}")
            # The private bus starts the bridge; the container sends the
            # notification to the server of the host.
            sc(env, "notify-send -u critical -i /etc/passwd -h string:x-dunst-stack-tag:host hello body")
            n = json.loads(client.succeed(f"cat {notif_log}").splitlines()[-1])
            assert n["app"] == f"{env}: notify-send", n
            assert n["summary"] == f"{env}: hello" and n["body"] == "body", n
            assert n["icon"] == "", n
            assert n["hints"].get("urgency") == 2 and n["hints"].get("frcolor") == color, n
            # Critical: the background of the host.
            assert "bgcolor" not in n["hints"], n
            assert "x-dunst-stack-tag" not in n["hints"], n
            # The server of the sandbox is the bridge, not the server of the
            # host.
            assert "sandbox" in sc(env, "notify-send --version >/dev/null; dbus-send --session --print-reply --dest=org.freedesktop.Notifications /org/freedesktop/Notifications org.freedesktop.Notifications.GetServerInformation"), env
            # A run without a container has no notifications.
            sb(env, "notify-send nope", ok=False)
        # notifications = false: an error, and nothing on the host.
        client.succeed(f"rm -f {notif_log}")
        sc("quiet", "notify-send nope", ok=False)
        client.fail(f"test -e {notif_log}")
        client.succeed("pkill -u alice -f fake-notifications")

    with subtest("color scheme of the host"):
        key = "/org/gnome/desktop/interface/color-scheme"
        scheme_file = "/tmp/host-color-scheme"
        client.succeed(f"echo 1 > {scheme_file} && chmod 644 {scheme_file}")
        client.succeed(f"su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus fake-portal {scheme_file} >/tmp/fake-portal.out 2>&1 &'")
        def dconf_read(env):
            return client.succeed(run + f"sandbox run {env} -- dconf read {key}'").strip()
        has_portal = "dbus-send --session --print-reply --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner string:org.freedesktop.portal.Desktop | grep -q true"
        client.wait_until_succeeds(run + has_portal + "'")
        for env in ["net", "gnet"]:
            client.succeed(run + f"sandbox run {env} -- sleep 304 >/dev/null 2>&1 &'")
            client.wait_until_succeeds(run + f"systemctl --user -q is-active sandbox@{env}.service'")
            # The value at the start, and each change of the host.
            for n, value in [(1, "prefer-dark"), (2, "prefer-light"), (0, "default")]:
                client.succeed(f"echo {n} > {scheme_file}")
                retry(lambda _: dconf_read(env) == f"'{value}'", timeout_seconds=30)
            # The sandbox cannot write to the directory of the host.
            sc(env, "touch /run/user/1000/sandbox-color-scheme/color-scheme", ok=False)
            sc(env, "pkill -xf sleep.304")
            client.wait_until_succeeds(f"! su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 systemctl --user -q is-active sandbox@{env}.service'")
        # followColorScheme = false (the default): the sandbox keeps its
        # scheme.
        client.succeed(f"echo 1 > {scheme_file}")
        assert dconf_read("quiet") == "", "quiet follows the host"
        sc("quiet", "test ! -e /run/user/1000/sandbox-color-scheme")
        client.succeed("pkill -u alice -f fake-portal")

    with subtest("cli"):
        names = client.succeed("su - alice -c 'sandbox list --names'").split()
        assert sorted(names) == sorted(conf["environments"]), names
        out = client.succeed("su - alice -c 'sandbox list'")
        assert any(l.startswith("gnet ") and "#800080" in l and "gvisor" in l for l in out.splitlines()), out
        out = client.succeed("su - alice -c 'sandbox show net'")
        assert "backend: bwrap" in out and "network: private" in out and "--unshare-user" in out, out
        out = client.succeed("su - alice -c 'sandbox backends'")
        assert "sandbox-backend-bwrap" in out and "sandbox-backend-gvisor" in out and "logged-backend" in out, out
        assert client.succeed("su - alice -c 'sandbox home net'").strip() == "/home/alice/.local/share/sandboxes/net/home"
        assert client.succeed("su - alice -c 'SANDBOX_TEST_PICK=^gnone sandbox pick'").strip() == "gnone"
        assert client.succeed("su - alice -c 'SANDBOX_TEST_PICK=^host sandbox pick --host'").strip() == "host"
        # --gui: gnone has no gui, so the menu does not get it.
        client.fail("su - alice -c 'SANDBOX_TEST_PICK=^gnone sandbox pick --gui'")

    with subtest("external backend"):
        # A backend of the configuration gets run, then start and exec.
        client.succeed("rm -f /tmp/logged-backend.log")
        assert sb("ext", "echo \\$SANDBOX_BACKEND").strip() == "logged"
        sc("ext", "true")
        client.wait_until_succeeds(f"! {run}systemctl --user -q is-active sandbox@ext.service'")
        modes = client.succeed("cat /tmp/logged-backend.log").split()
        assert modes == ["run", "start", "exec"], modes

    with subtest("socks proxy"):
        # All traffic goes through the proxy on the server: nginx sees the
        # proxy, not the client.
        server.wait_for_open_port(1080, "192.168.1.2")
        for f in [sb, sc]:
            f("socks", inet)
            remote = server.succeed("tail -n 1 /var/log/nginx/access.log").split()[0]
            assert remote == "203.0.113.10", remote
        client.wait_until_succeeds(f"! {run}systemctl --user -q is-active sandbox@socks.service'")
        # Without the proxy, no connection: there is no other way out. (The
        # proxy host itself is reachable directly.)
        server.systemctl("stop microsocks")
        sb("socks", inet, ok=False)
        server.systemctl("start microsocks")

    with subtest("privacy filter"):
        # The proxy on 127.0.0.1:1080 of the host.
        client.succeed("su - alice -c 'microsocks -i 127.0.0.1 -p 1080 >/dev/null 2>&1 &'")
        client.wait_for_open_port(1080)
        sb("privacy", inet)
        client.succeed("pkill -x microsocks")
        sb("privacy", inet, ok=False)

    with subtest("signals"):
        # TERM to a run stops its program: in a scope (in the container), and
        # without a scope (bwrap and gVisor, each in a network of its own).
        client.succeed(run + "sandbox run net -- sleep 311 >/dev/null 2>&1 &'")
        client.wait_until_succeeds("pgrep -u alice -xf 'sleep 311'")
        front = client.succeed("pgrep -u alice -f 'sandbox run net -- sleep 311'").split()[0]
        client.succeed(f"kill -TERM {front}")
        client.wait_until_fails("pgrep -u alice -xf 'sleep 311'")
        client.wait_until_succeeds(f"! {run}systemctl --user -q is-active sandbox@net.service'")
        for env in ["net", "gnet"]:
            client.succeed(f"su - alice -c 'SANDBOX_NO_SCOPE=1 sandbox run {env} -- sleep 312 >/dev/null 2>&1 &'")
            front = client.wait_until_succeeds(f"pgrep -u alice -f 'sandbox run {env} -- sleep 312'").split()[0]
            # The program of gVisor is in the Sentry of runsc, not a process
            # of the host.
            if env == "net":
                client.wait_until_succeeds("pgrep -u alice -xf 'sleep 312'")
            else:
                client.wait_until_succeeds("pgrep -u alice -f 'runsc.*sandbox-gnet'")
                client.sleep(2)
            client.succeed(f"kill -TERM {front}")
            client.wait_until_fails(f"pgrep -u alice -f 'sandbox run {env} -- sleep 312'")
            if env == "net":
                client.wait_until_fails("pgrep -u alice -xf 'sleep 312'")
            else:
                client.wait_until_fails("pgrep -u alice -f 'runsc.*sandbox-gnet'")

    with subtest("concurrent first runs"):
        # Two runs start the container at the same time: one container.
        for _ in range(2):
            client.succeed(run + "sandbox run lanonly -- sleep 313 >/dev/null 2>&1 &'")
        client.wait_until_succeeds(run + "sandbox run lanonly -- sh -c \"test \\$(pgrep -cxf sleep.313) = 2\"'")
        sc("lanonly", "pkill -xf sleep.313")
        client.wait_until_succeeds(f"! {run}systemctl --user -q is-active sandbox@lanonly.service'")

    with subtest("a container that stops"):
        # When the container stops, the programs of its runs stop, and the
        # next run starts a new container.
        client.succeed(run + "sandbox run net -- sleep 314 >/dev/null 2>&1 &'")
        client.wait_until_succeeds("pgrep -u alice -xf 'sleep 314'")
        client.succeed(run + "systemctl --user kill --signal=KILL sandbox@net.service'")
        client.wait_until_fails("pgrep -u alice -xf 'sleep 314'")
        assert sc("net", "echo again").strip() == "again"

    if ${if opensnitchOverlay != null then "True" else "False"}:
        with subtest("opensnitch"):
            # The daemon of the sandbox sees the connections of the sandbox:
            # the rule of the sandbox denies 203.0.113.10, the default action
            # allows the rest.
            client.succeed(run + "sandbox run osn -- sleep 315 >/dev/null 2>&1 &'")
            client.wait_until_succeeds(f"{run}systemctl --user -q is-active sandbox@osn.service'")
            client.succeed("pgrep -u alice -f 'opensnitchd -config-file'")
            sc("osn", lan)
            sc("osn", inet, ok=False)
            # The rule is a link into the store in the directory of the daemon.
            client.succeed("test -L /home/alice/.local/share/sandboxes/osn/opensnitch/rules/deny-inet.json")
            # The daemon starts again after it stops. (The package wraps the
            # program: its process name is not opensnitchd.)
            client.succeed("pkill -u alice -f 'opensnitchd -config-file'")
            # The daemon removes its queue rules when it stops; the namespace
            # process sees it and refuses all connections until the new
            # daemon is ready.
            client.wait_until_succeeds(run + "journalctl --user -u sandbox@osn.service | grep -q \"no connections until it is back\"'")
            client.wait_until_succeeds("pgrep -u alice -f 'opensnitchd -config-file'")
            client.wait_until_succeeds(run + f"sandbox run osn -- sh -c \"{lan}\"'")
            sc("osn", inet, ok=False)
            client.succeed(run + "sandbox stop osn'")
            # A run without a scope has a daemon of its own and no restart:
            # after its daemon stops, the run has no network. (The daemon
            # needs about 6 s to stop.)
            result = "/home/alice/.local/share/sandboxes/osn/home/result"
            client.succeed(f"rm -f {result}")
            client.succeed("su - alice -c 'SANDBOX_NO_SCOPE=1 sandbox run osn -- sh -c \"" + lan + " && echo before > ~/result; sleep 12; " + lan.replace("-m 5", "-m 3") + " && echo got >> ~/result || echo refused >> ~/result\" >/dev/null 2>&1 &'")
            client.wait_until_succeeds(f"grep -q before {result}")
            client.succeed("pkill -u alice -f 'opensnitchd -config-file'")
            client.wait_until_succeeds(f"grep -qx 'refused\\|got' {result}", timeout=60)
            assert client.succeed(f"cat {result}").split() == ["before", "refused"], client.succeed(f"cat {result}")

    with subtest("scopes"):
        client.succeed(run + "sandbox run net -- sleep 300 >/dev/null 2>&1 &'")
        client.wait_until_succeeds(run + "sandbox ps' | grep -q '^net .*sleep 300'")
        # The run is in the declared slice of the sandbox. A window manager
        # reads the sandbox name from this cgroup path.
        assert "Sandbox net" in client.succeed(run + "systemctl --user show -p Description sandbox-net.slice'")
        # The scope is there before the program: the private network starts
        # first.
        pid = client.wait_until_succeeds("pgrep -u alice -xf 'sleep 300'").split()[0]
        cgroup = client.succeed(f"cat /proc/{pid}/cgroup")
        assert "/sandbox.slice/sandbox-net.slice/sandbox-net-" in cgroup, cgroup
        # The container is in the same slice.
        assert "sandbox-net.slice" in client.succeed(run + "systemctl --user show -p Slice sandbox@net.service'")
        client.succeed(run + "sandbox stop net'")
        client.wait_until_fails("pgrep -u alice -xf 'sleep 300'")
        assert not active("net")

    with subtest("host"):
        # The program leaves the cgroup of the caller for its own scope below
        # app.slice, with the label of the host.
        client.succeed(run + "sandbox host -- sleep 301 >/dev/null 2>&1 &'")
        pid = client.wait_until_succeeds("pgrep -u alice -xf 'sleep 301'").split()[0]
        cgroup = client.succeed(f"cat /proc/{pid}/cgroup")
        assert "/app.slice/app-sandbox-host-" in cgroup, cgroup
        env = client.succeed(f"tr '\\0' '\\n' < /proc/{pid}/environ")
        assert "SANDBOX_NAME=host" in env, env
        # A run from this program must start its own scope.
        assert "SANDBOX__SCOPED" not in env, env
        client.succeed("pkill -u alice -xf 'sleep 301'")
  '';
}
