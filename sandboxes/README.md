# sandboxes

Named sandboxes for programs on NixOS, as the qubes of Qubes OS. Each sandbox
has a name, a color, a home directory, a network policy and a set of
capabilities. The `sandbox` command starts a program in a sandbox with a
**backend**: bubblewrap, gVisor, or a backend of your own.

The options use the configuration keys of
[sandboxed-agents](https://github.com/eordano/sandboxed-agents#configuration).

| Qubes OS                 | sandboxes                                                     |
| ------------------------ | ------------------------------------------------------------- |
| qube                     | `programs.sandboxes.environments.<name>`                      |
| label color              | `color`; the window manager draws it                          |
| AppVM private volume     | `persistentHome = true` (`~/.local/share/sandboxes/<name>`)   |
| disposable VM            | `persistentHome = false`                                      |
| TemplateVM root          | the read-only `/nix/store` and `/etc` of the host             |
| NetVM / firewall         | slirp4netns + iptables per sandbox; `internetAccess`, `lanAccess`, `allowHosts`, `socksProxy` |
| firewall prompts         | an OpenSnitch daemon in the network of the sandbox (`opensnitch.enable`) |
| `qvm-ls`                 | `sandbox list`, `sandbox ps`                                  |
| `qvm-run`                | `sandbox run NAME -- CMD`                                     |
| `qvm-shutdown`           | `sandbox stop NAME`                                           |

## Usage

The module is part of [mzde](../README.md): `mzde.nixosModules.default`
imports it, and gives the desktop around it (`nix/sandboxes.nix`). Without the
desktop, import only `mzde.nixosModules.sandboxes`:

```nix
{
  inputs.mzde.url = "git+https://github.com/menduz/mzde.git?ref=main";

  outputs = { nixpkgs, mzde, ... }: {
    nixosConfigurations.box = nixpkgs.lib.nixosSystem {
      modules = [
        mzde.nixosModules.sandboxes
        {
          programs.sandboxes = {
            enable = true;
            environments = {
              work = {
                color = "#008080";
                ssh = true;
                git = true;
                paths = [ "~/code" ];
              };
              web = {
                color = "#800080";
                backend = "gvisor";
                audio = true;
                lanAccess = false;
              };
              vault = {
                color = "#599764";
                disableNetworking = true;
              };
            };
          };
        }
      ];
    };
  };
}
```

```console
$ sandbox list
work      #008080   bwrap   internet   gui,ssh,git        ...
web       #800080   gvisor  inet-only  gui,audio          ...
vault     #599764   bwrap   none       gui                ...
$ sandbox run work -- ghostty
$ sandbox run web -- firefox
$ sandbox ps
SANDBOX      PID      COMMAND
work         41213    ghostty
$ sandbox stop work
$ sandbox logs work -f      # the log of the container: journalctl --user -u sandbox@work
$ sandbox show web          # the plan and the backend command, not started
$ sandbox pick --host       # a menu (rofi by default); prints the name
$ sandbox pick --gui        # only the sandboxes with gui
$ sandbox host -- ghostty   # no sandbox, but SANDBOX_NAME=host, the host color
                            # and a scope app-sandbox-host-*.scope below app.slice
$ sandbox current           # the sandbox of this process, or "host"
$ sandbox run --need-cwd work -- make
                            # stops if the sandbox does not have the current directory
```

### Commands

A program can run only in sandboxes. `commands` of a sandbox gives the
system and the sandbox a command for each program:

```nix
programs.sandboxes.environments.work.commands = {
  make = pkgs.gnumake;
  claude = {
    package = pkgs.claude-code;
    cwdOnly = true;
  };
};
```

- In `work`, `claude` starts the program. The command is first in PATH.
- On the host and in the sandboxes without the command, `claude` stops with
  an error: start it in a terminal of a sandbox with it (`sandbox run work`).
- Many sandboxes can have the same command. In `programs.sandboxes.defaults`,
  a command is in each sandbox, or in each sandbox that allows it:

  ```nix
  programs.sandboxes.defaults = { config, ... }: {
    commands = lib.mkIf (config.claude != "none") {
      claude = { package = pkgs.claude-code; cwdOnly = true; };
    };
  };
  ```
- With `cwdOnly`, the program sees only the current directory of the
  `paths` of the sandbox: in `~/code/app`, `~/code` has only `app`, and a
  file of `paths` is empty. The other mounts of the sandbox stay. If a path
  has the configuration of a program (`~` with `claude = "write"`), the
  command does not start. The program runs in a nested bwrap, so
  the sandbox needs user namespaces (the bwrap backend). `bwrapArgs` gives
  more arguments to that bwrap.

The command asks `sandbox current`: that is `SANDBOX_NAME`, but a program on
the host can set `SANDBOX_NAME`. On the host PID 1 is systemd, and then
`sandbox current` is `host`. In a sandbox, PID 1 is bwrap (bwrap) or the
first program (gVisor).

## How it works

Each sandbox is a container, as in Docker. The first run starts it, each run
starts its program in it (as `docker exec`), and it stops after the last run.

```
sandbox run NAME -- CMD
 └─ systemd-run --user --scope --slice=sandbox-NAME.slice     (sandbox ps / stop)
     │  Requires=sandbox@NAME.service: systemd starts the container first
     └─ the frontend makes the plan of the run (plan.json)
         └─ <backend> exec plan.json: CMD in the container, with the
             environment, the directory and the terminal of this run

sandbox@NAME.service: the container of NAME, for all runs   (in sandbox-NAME.slice)
 │  Type=notify: the scope of a run starts when the container is ready
 ├─ network "none" or "host": the backend starts at once
 ├─ network "private": a user and network namespace, slirp4netns, iptables,
 │   the OpenSnitch daemon (opensnitch.enable)
 └─ <backend> start plan.json
     ├─ bwrap:  bubblewrap, the kernel of the host
     ├─ gvisor: runsc, an OCI bundle, the kernel of gVisor
     └─ sandbox-init: the X server (xwayland-satellite) and the session bus
         of the sandbox; it stays until the container stops
```

The frontend does all the work that does not change with the backend: it
reads the configuration, finds the sockets of the host, selects the mounts and
the environment, and makes the network namespace. A backend only isolates.

`sandbox` is one program in Rust (`src/`), with no shell script at run time.
The names `sandbox-init`, `sandbox-backend-bwrap` and
`sandbox-backend-gvisor` are links to it. It starts other programs (bwrap,
runsc, slirp4netns, iptables, systemd-run, ...) with their absolute paths
from the build or the configuration, never from the PATH of the caller, and
with one argument for each value (no shell). `cargo test` in this directory
runs the unit tests.

- All runs of a sandbox share the processes, `/tmp`, `$XDG_RUNTIME_DIR`, the
  home directory of a disposable sandbox, the X server, the session bus, the
  network and the OpenSnitch daemon. A sandbox has one of each.
- The container gets the mounts and the environment of the run that starts
  it (for example `SSH_AUTH_SOCK`); the run writes
  only the variables that the plan reads to
  `$XDG_RUNTIME_DIR/sandboxes/NAME/caller.json`. A run whose configuration
  gives the same or more access than the container (a new path, a new store
  path after a rebuild, a different socket) starts its program with the
  mounts of the container, and `sandbox` tells it. A run whose configuration
  gives less access (a path that is not in `paths` now, rw that is now ro, a
  different source) stops with an error. `sandbox stop NAME` stops the
  container and its programs, and the next run starts it again. `sandbox ps`
  marks a container that started with a different configuration.
- A program of a run can stay after the command of the run (for example a
  server that the command starts). The scope of the run, and thus the
  container, stay until its last process stops. A run of bwrap stops its
  command if `sandbox` stops.
- A run without a scope (`SANDBOX_NO_SCOPE=1`, or no systemd user manager)
  gets a container of its own: `<backend> run plan.json`, with its own
  network, X server and session bus.
- The service has the log of the container and of the OpenSnitch daemon:
  `sandbox logs NAME` (`journalctl --user -u sandbox@NAME`; more arguments go
  to journalctl, for example `sandbox logs NAME -f`).
- The service sends a notification when the container starts and when it
  stops (`programs.sandboxes.notifications`, `sandbox __notify`, src/notify.rs).
  A stop after a signal (for example SIGKILL), the OOM killer or an error
  gives a critical notification with the reason, from the variables of
  ExecStopPost (SERVICE_RESULT, EXIT_STATUS).
- A run with a terminal gets a terminal of its own, as `docker exec -t`:
  `sandbox` copies the bytes between it and the terminal of the caller, so a
  program in a sandbox cannot put characters into the input of the terminal
  of the caller. With bwrap, the terminal is on the devpts of the container,
  so `tty` (and `GPG_TTY`) finds its name. The module also sets
  `dev.tty.legacy_tiocsti = 0` (mkDefault).
- Nested sandboxes are forbidden. In a sandbox, `run`, `host`, `stop` and
  the backends (`run`, `start`, `exec`) stop with an error, also for the
  same sandbox and without SANDBOX_NAME: in a sandbox of bwrap, PID 1 is
  bwrap. `host` there would give the label of the host to a program of the
  sandbox. `list`, `show`, `current` and the `commands` work. Nested bwraps
  and namespaces stay possible (cwdOnly uses them): they can only remove
  access. This check is not a security boundary: a program that goes around
  it (a PID namespace of its own and no SANDBOX_NAME) gets a sandbox in its
  sandbox, with no more access than its sandbox. The boundary between
  sandboxes is what each sandbox can open.

## Options

Each sandbox in `programs.sandboxes.environments.<name>`:

| Option                   | Default   | Function |
| ------------------------ | --------- | -------- |
| `description`            | `""`      | text for `sandbox list` and the menu |
| `color`                  | `#808080` | label color, also in `SANDBOX_COLOR` |
| `gtkTheme`               | `null`    | `GTK_THEME` of the programs |
| `backend`                | `bwrap`   | a name of `programs.sandboxes.backends` (`runsc` is `gvisor`) |
| `backendArgs.<backend>`  | `[]`      | extra arguments for one backend |
| `extraBubblewrapArgs`    | `[]`      | the same as `backendArgs.bwrap` |
| `extraRunscArgs`         | `[]`      | the same as `backendArgs.gvisor` |
| `extraQemuArgs`          | `[]`      | the same as `backendArgs.microvm` |
| `extraSandboxExecArgs`   | `[]`      | the same as `backendArgs.seatbelt` |
| `hostname`               | the name  | host name in the sandbox |
| `persistentHome`         | `true`    | the home directory keeps its files |
| `homeDirectory`          | `<stateDir>/<name>/home` | the home directory on the host |
| `homePatterns`           | `[]`      | paths below `$HOME` from the host, read-only (`rw:` prefix: writable) |
| `files.<path>`           | `{}`      | read-only files below `$HOME` (`text` or `source`) |
| `paths`                  | `[]`      | bind mounts: `/p`, `ro:/p`, `/host:/guest` |
| `env`                    | `{}`      | extra environment variables |
| `extraEnvs`              | `[]`      | extra environment variables as `"KEY=VALUE"` |
| `passEnv`                | `[]`      | more variables from the caller (added to `programs.sandboxes.passEnv`) |
| `packages`               | `[]`      | packages at the start of `PATH` |
| `commands`               | `{}`      | programs that run only in this sandbox: a command of the system for each (refer to Commands) |
| `ssh`                    | `false`   | `~/.ssh` and the agent socket (an access, see below) |
| `sshWrite`               | `false`   | the same as `ssh = "write"` |
| `gpg`                    | `false`   | `~/.gnupg` and the gpg-agent sockets |
| `git`                    | `false`   | `~/.gitconfig`, `~/.config/git`, `~/.git-credentials` |
| `claude`                 | `false`   | `~/.claude`, `~/.claude.json` |
| `codex`                  | `false`   | `~/.codex` |
| `gemini`                 | `false`   | `~/.gemini` |
| `hermes`                 | `false`   | `~/.hermes` |
| `opencode`               | `false`   | `~/.config/opencode` |
| `zed`                    | `false`   | `~/.config/zed`, `~/.local/share/zed` |
| `direnv`                 | `false`   | `~/.config/direnv`, `~/.local/share/direnv` (the allowed `.envrc` files) |
| `xdgRemap`               | `false`   | `$XDG_CONFIG_HOME/<program>` at `~/.<program>` (claude, codex, gemini, hermes), if it is on the host |
| `noXdgRemap`             | `false`   | `xdgRemap` is false |
| `yolo`                   | `false`   | `claude` starts with `--dangerously-skip-permissions` |
| `docker`                 | `false`   | the Docker socket (root on the host) |
| `fuse`, `kvm`, `nvidia`  | `false`   | the devices |
| `gui`                    | `true`    | a Wayland socket of a security context (no privileged protocols), `/dev/dri`, `/sys` |
| `x11`                    | `private` | `private`: an X server for the sandbox (xwayland-satellite); `host`: the X sockets of the host (no isolation); `none` |
| `audio`                  | `false`   | PipeWire, PulseAudio, `/dev/snd` |
| `dbus`                   | `private` | `private`: a session bus for the sandbox; `host`: the session bus of the host (no isolation); `none` |
| `libvirt`                | `false`   | the libvirt sockets |
| `nix`                    | `false`   | the store of the host through the Nix daemon, as an untrusted client (`nix-daemon --force-untrusted --process-ops`), and `~/.cache/nix` of the host, writable (the sandbox can write the eval cache of the host) |
| `allowHome`              | `false`   | the real home directory, writable |
| `internetAccess`         | `true`    | false: only local networks and `allowHosts` |
| `lanAccess`              | `true`    | false: only the internet |
| `allowHosts`             | `[]`      | hosts that are open when `internetAccess` is false |
| `disableNetworking`      | `false`   | no network |
| `shareHostNetwork`       | `false`   | the network namespace of the host (no firewall; the sandbox can reach 127.0.0.1 and abstract sockets such as Xwayland) |
| `allowHostLoopback`      | `false`   | 127.0.0.1 of the host at 10.0.2.2 |
| `socksProxy`             | `null`    | all traffic through a SOCKS5 proxy (tun2socks); `HOST:PORT` or `socks5://HOST:PORT` |
| `privacyFilter`          | `false`   | all traffic through the privacy-filter proxy: `socksProxy`, else port 1080 of the host |
| `mountHomeCache`         | `false`   | `programs.sandboxes.homeCacheDirs` |
| `mountCommonHomeFolders` | `false`   | `programs.sandboxes.commonHomeFolders` |
| `mountTmp`               | `false`   | the `/tmp` of the host |
| `sharedTmp`              | `false`   | `/tmp/sandbox-<uid>-<name>` of the host at `/tmp`, in place of a tmpfs: the files stay after the container, and the runs without a scope get them too |
| `mounts`                 | `[]`      | `"common-tools"`: `mountCommonHomeFolders`; `"caches"`: `mountHomeCache` |
| `cleanTmp`               | `true`    | remove the files of the run (the plan) after the program |
| `opensnitch.enable`      | `false`   | an OpenSnitch daemon in the network of the sandbox |
| `opensnitch.rules`       | `{}`      | OpenSnitch rules for this sandbox only |
| `opensnitch.allowOnHost` | `false`   | the daemon of the host allows the slirp4netns of this sandbox |
| `opensnitch.directory`   | `<stateDir>/<name>/opensnitch` | the configuration and the rules of the daemon |

The options `ssh`, `git`, `claude`, `codex`, `gemini`, `hermes`, `opencode`,
`zed` and `direnv` are accesses to the configuration of a program on the host:

| Value             | Function |
| ----------------- | -------- |
| `"none"`, `false` | the sandbox does not get it |
| `"read"`, `true`  | read-only |
| `"write"`         | writable; the command makes a directory that is not on the host |

```nix
agents = {
  claude = "write";
  codex = "read";
  git = "read";
  zed = "read";
  ssh = true;
};
```

A path below `.config`, `.local/share` or `.cache` comes from
`$XDG_CONFIG_HOME`, `$XDG_DATA_HOME` or `$XDG_CACHE_HOME` of the host. A path
that is not on the host is not mounted. The mounts of these options come after
`homePatterns` and `files`, so they cover a path of `homePatterns` or `files`
below them.

`programs.sandboxes.host.color` is the label color of the host: `sandbox host`
and the "host" entry of `sandbox pick --host` use it. No sandbox can have the
name `host`.

`programs.sandboxes.defaults` is a module that applies to every sandbox. It
gets `name` and `config` of the sandbox, and it can declare options, for
example a theme that it makes from the color.

## OpenSnitch

With `opensnitch.enable`, the sandbox has an OpenSnitch daemon in its network
namespace. The daemon sees each connection before slirp4netns, so it sees the
program in the sandbox (for example `firefox`), and not `slirp4netns`. It asks
the OpenSnitch GUI of the host, as the daemon of the host does.

```
sandbox run web -- firefox                 sandbox run web -- curl ...
 └─ scope (Requires=sandbox@web)            └─ scope (Requires=sandbox@web)
     └─ nsenter ──┐                             └─ nsenter ──┐
                  ▼                                          ▼
sandbox@web.service: the container of "web", one network namespace for all runs
 ├─ opensnitchd (proc monitor, nftables queue) ─── unix socket ──► GUI: node unix:/web
 ├─ iptables of internetAccess / lanAccess
 ├─ slirp4netns ──► the network of the host (and its OpenSnitch daemon)
 └─ <backend> start: the container
```

- All runs of the sandbox share the network namespace of the container, so
  the sandbox has one daemon and one node in the GUI. The user service
  `sandbox@<name>` holds the namespace. Each run requires it, so systemd
  starts it before the first run and stops it after the last run
  (`StopWhenUnneeded`). A run without a scope (`SANDBOX_NO_SCOPE=1`) gets a
  namespace and a daemon of its own.
- The daemon sends its name to the GUI (`Server.Name`). The GUI shows it as
  the node `unix:/<name>`: the pop-up tells the node ("curl on **web** is
  connecting to ..."), and the rules page shows the rules of each node. The
  GUI shows one pop-up for each node, so the pop-ups of two sandboxes can be
  on the screen at the same time.
- The rules of `programs.sandboxes.opensnitch.rules` apply to all daemons of
  the sandboxes. The default is `services.opensnitch.rules`: a rule of the
  configuration applies to the host and to each sandbox. The rules that you
  make in the GUI are files in `opensnitch.directory/rules`, and they stay.
- The daemon runs as the user, as root of a user namespace. eBPF and audit
  need root on the host, so the daemon uses the `proc` process monitor.
- The queue has no bypass: a daemon that crashes leaves its queue rules,
  and no connection goes out. A daemon that a program of the host stops
  (SIGTERM) removes its queue rules: when the namespace process sees that
  the daemon stopped, it puts a rule that rejects all connections first in
  OUTPUT, starts the daemon again, and removes the rule when the queue rules
  of the new daemon are in place. Between the end of the rules and that
  moment, connections can go out. A program in the sandbox cannot stop the
  daemon: the daemon is in a different user namespace and PID namespace. A
  run without a scope has no restart: when its daemon stops, the run has no
  network until it stops.
- The configuration is the configuration of the daemon of the host, with the
  changes for a user namespace, and `programs.sandboxes.opensnitch.settings`.
  The command writes it again at each start, so a change from the GUI lasts
  until the next start.
- The daemon of the host sees `slirp4netns`. `opensnitch.allowOnHost` adds a
  rule that allows the `slirp4netns` of the sandbox (by `SANDBOX_NAME` in its
  environment), so that the host does not ask a second time.

The daemon must be the fork of menduz: it opens its netfilter queue in a user
namespace, and it sends `Server.Name` to the GUI. The GUI must also be the
fork, to show the nodes by name.

```console
$ sandbox logs web                         # the log of the daemon
$ ls ~/.local/share/sandboxes/web/opensnitch/rules
```

## Backends

A backend is a command with four forms:

```
<backend> run   PLAN.json   start the program of the plan in a container of its own; the exit code is the exit code of the program
<backend> start PLAN.json   start the container of a sandbox, with the command of the plan as its first program;
                            make the file "ready" in the directory .state when `exec` can start programs;
                            stop when the container stops
<backend> exec  PLAN.json   start the command of the plan in the container of .state, with .env, .cwd and .tty;
                            the exit code is the exit code of the program
<backend> show  PLAN.json   print the command, and do not start it
```

`exec` uses only `.state`, `.command`, `.env`, `.cwd`, `.tty` and
`.network` of its plan: the mounts are the mounts of the container.

Add a backend with `programs.sandboxes.backends.<name> = package;` (the
package must have `meta.mainProgram`), then use `backend = "<name>";`.

| Backend  | Isolation                       | Limits |
| -------- | ------------------------------- | ------ |
| `bwrap`  | Linux namespaces                | the program uses the kernel of the host |
| `gvisor` | a kernel in user space (runsc)  | no Wayland or X11 client can draw: gVisor cannot give its memory files (the window buffers) to the compositor, so use `gui = false` and a terminal on the host; no `/dev/dri`, `/dev/snd`, `/dev/kvm`; NVIDIA through `--nvproxy` (`SANDBOX_RUNSC_NVPROXY=1`); the program is uid 0 in gVisor (rootless runsc maps it to the user on the host) |

`exec` of bwrap enters the namespaces and the root of the container in one
process: it checks the process of `.state/pid` with a pidfd and its start
time, enters the parent user namespace, the other namespaces and then the
user namespace of the container, and starts the program without
capabilities, with `no_new_privs` and the parent death signal, as a program
that bwrap starts. With a terminal, it opens a terminal on the `/dev/ptmx` of
the container (the container has a devpts of its own, and the terminal of the
caller has no name there), and copies the bytes between the two terminals.
`exec` of gVisor is `runsc exec`.

With a terminal (`tty` in the plan), the gVisor backend runs the command in
`script` (util-linux). `script` makes a terminal in gVisor, so Ctrl-C and a
change of the window size go to the program in the sandbox, and not to runsc.
The backend sends SIGWINCH to the processes of the sandbox when the terminal
of the host changes its size.

### The plan

```json
{
  "version": 1,
  "name": "work",
  "hostname": "work",
  "uid": 1000,
  "gid": 100,
  "home": "/home/alice",
  "cwd": "/home/alice/code",
  "tty": false,
  "network": "inherit",
  "state": "/run/user/1000/sandboxes/work/c-0123abcd/backend",
  "mounts": [
    { "type": "bind",  "source": "/nix/store", "target": "/nix/store", "mode": "ro" },
    { "type": "bind",  "source": "/home/alice/.local/share/sandboxes/work/home", "target": "/home/alice", "mode": "rw" },
    { "type": "dev",   "source": "/dev/dri", "target": "/dev/dri", "mode": "rw" },
    { "type": "tmpfs", "source": "", "target": "/run/user/1000", "mode": "0700" }
  ],
  "env": { "SANDBOX_NAME": "work", "SANDBOX_COLOR": "#008080", "PATH": "..." },
  "command": ["ghostty"],
  "extraArgs": []
}
```

- `network`: `none` (the backend makes an empty network namespace) or
  `inherit` (the backend uses the network namespace that it starts in).
- `state`: for `start` and `exec`, the directory of the container, for the
  files of the backend; empty for `run`.
- `mounts`, in order; a later mount can cover an earlier one.
  - `bind`: `mode` is `ro` or `rw`.
  - `dev`: a device node or directory of devices.
  - `tmpfs`: `mode` is the octal permission.
- `env`: the full environment. The backend adds nothing.
- The backend gives `/proc`, `/dev` and `/dev/shm` itself.
- `command` starts with `sandbox-init` when the sandbox has a private X
  server or a private session bus. `sandbox-init` starts them, and then it
  replaces itself with the command. In the plan of `start`, `sandbox-init`
  starts them and stays; in the plan of `exec`, it gets their variables
  (`DISPLAY`, `DBUS_SESSION_BUS_ADDRESS`) and then replaces itself with the
  command. A backend does nothing special for it.

## Window managers

The module declares a systemd user slice for each sandbox,
`sandbox-<name>.slice` below `sandbox.slice`. `sliceConfig` sets its limits
(`MemoryMax=`, `CPUQuota=`). Every run is in a scope in that slice, and the
container (with the X server of the sandbox) is the service `sandbox@<name>`
in that slice, so a window manager finds the sandbox of a window from the
cgroup of the client PID:

1. the cgroup `.../sandbox.slice/sandbox-<name>.slice/...`: the sandbox
   `<name>`, with the color of `/etc/sandboxes/config.json`;
2. else the host, with `.host.color` of `/etc/sandboxes/config.json`.

The window manager does not read the environment: a program can change its
own environment, but not its cgroup. With gVisor, the Wayland client is the
Sentry of runsc, so only the cgroup is correct. Thus a run without a scope
(`SANDBOX_NO_SCOPE=1`) is on the host for the window manager. `SANDBOX_NAME`
and `SANDBOX_COLOR` are only for the programs in the sandbox, for example a
shell prompt. The kwm fork of menduz and `netgraph.sh` do this.

## Tests

- `cargo test` (in this directory) runs the tests of the program; the Nix
  build of the package runs them too.
  - unit tests (`src/`): the plan for each option and mode on a fake host
    (`src/host.rs`, `src/plan_tests.rs`), the arguments of bwrap and runsc,
    the firewall and the proxy of the network, the OpenSnitch configuration
    and rules, the commands and cwdOnly, the menu, the comparison of the
    mounts, the pty, the processes and the signals;
  - integration tests (`tests/`): the program itself, with a test
    configuration: the subcommands, the bwrap backend (run, start, exec),
    `sandbox run`, the commands, `host`, `yolo` and sandbox-init. The tests
    that need user namespaces do not run where bwrap cannot make a sandbox.
- `nix build .#checks.x86_64-linux.sandboxes` (in mzde) runs a NixOS VM
  test: the network modes, the SOCKS proxy and the privacy filter, the home
  directories, the commands, the label, the scopes, the containers (shared
  runs, refused and stale configurations, concurrent starts, a container
  that stops), the signals, an external backend, OpenSnitch, and the menu,
  with both backends.

## Limits of this prototype

- The Wayland socket of a sandbox is a socket of a security context
  (`wp_security_context_v1`, src/wayland.rs), never the socket of the
  compositor. The container (or a run without a scope) makes it when it
  starts, and the compositor accepts connections on it until the container
  stops. river does not give the privileged protocols to its clients: no
  virtual keyboard or pointer, no capture of the screen, no data control of
  the clipboard (a clipboard tool such as `wl-paste` needs the focus), no
  window manager, layer shell or lock of the session. A compositor without
  `wp_security_context_manager_v1` gives no Wayland socket to the
  sandboxes. A restart of the compositor stops the security contexts: the
  windows of the sandboxes stop, and `sandbox stop NAME` is necessary for
  new windows. A container of an older version has the socket of the
  compositor: a run refuses to join it.
- No D-Bus filter to the host: `dbus` gives a private bus or all of the
  session bus of the host. `xdg-dbus-proxy` and the portals are the next step.
- The mounts of a container do not change while it runs. A new
  configuration needs a new container (`sandbox stop NAME`). A smaller access applies to the running
  programs only after `sandbox stop NAME`.
- A program that a run of gVisor starts in the background is in gVisor,
  not in the scope of the run: it does not keep the container, and it stops
  when the container stops after the last run.
- Resource limits apply to the whole sandbox (`sliceConfig`), not to one run.
- The runs of a sandbox share the network namespace: they can connect to
  each other on 127.0.0.1.
