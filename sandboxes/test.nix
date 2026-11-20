# A NixOS VM test of the network modes, the home directory and the label
# environment. The node "server" has a local address (192.168.1.2) and an
# address that is not local (203.0.113.10, "the internet").
{ module }:
{ pkgs, ... }:
let
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
in
{
  name = "sandboxes";

  nodes.server = {
    networking.firewall.allowedTCPPorts = [ 80 ];
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
      pkgs.curl
      pkgs.socat
    ];
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
      linger = true;
    };
    programs.sandboxes = {
      enable = true;
      environments = {
        net = {
          color = "#008080";
          commands = { inherit whereami; };
        };
        lanonly.internetAccess = false;
        inetonly.lanAccess = false;
        allowlisted = {
          internetAccess = false;
          allowHosts = [ "203.0.113.10" ];
        };
        none.disableNetworking = true;
        hostnet.shareHostNetwork = true;
        disposable.persistentHome = false;
        sharedtmp.sharedTmp = true;
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
        };
        glanonly = {
          backend = "gvisor";
          internetAccess = false;
        };
        gnone = {
          backend = "gvisor";
          disableNetworking = true;
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
        # A service on 127.0.0.1 of the host, as a Gradle daemon.
        client.succeed("su - alice -c 'socat TCP-LISTEN:9000,bind=127.0.0.1,fork,reuseaddr SYSTEM:\"echo hi\" >/dev/null 2>&1 &'")
        client.wait_for_open_port(9000)
        for env, ok in [("hostnet", True), ("ghostnet", True), ("net", False), ("gnet", False)]:
            sb(env, "socat -T 3 - TCP:127.0.0.1:9000 </dev/null | grep -q hi", ok)

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
        sb("agents", "grep -q dangerously-skip-permissions \\$(command -v claude)")

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
        # cwdOnly does not start when a path has the configuration of a program.
        out = sb("homeagent", "cd ~/work/a && whereami-cwd 2>&1", ok=False)
        assert "has the configuration /home/alice/.claude" in out, out

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

    with subtest("scopes"):
        run = "su - alice -c 'XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus "
        client.succeed(run + "sandbox run net -- sleep 300 >/dev/null 2>&1 &'")
        client.wait_until_succeeds(run + "sandbox ps' | grep -q '^net .*sleep 300'")
        # The run is in the declared slice of the sandbox. A window manager
        # reads the sandbox name from this cgroup path.
        assert "Sandbox net" in client.succeed(run + "systemctl --user show -p Description sandbox-net.slice'")
        # The scope is there before the program: the private network starts
        # first.
        pid = client.wait_until_succeeds("pgrep -u alice -x sleep").split()[0]
        cgroup = client.succeed(f"cat /proc/{pid}/cgroup")
        assert "/sandbox.slice/sandbox-net.slice/sandbox-net-" in cgroup, cgroup
        client.succeed(run + "sandbox stop net'")
        client.wait_until_fails("pgrep -u alice -x sleep")

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
