# Migration of the sandbox orchestration to Rust

An analysis of all behavior that the bash code of `sandboxes/` had, of the
interfaces that other programs use, and of what a Rust binary changes for
security.

Status: done for the run time. `sandbox.sh`, `init.sh`, `init.nix`,
`backends/bwrap.sh`, `backends/gvisor.sh` and the generated bash of the
commands and of `yolo` are replaced by `src/` (one program; `sandbox-init`
and `sandbox-backend-*` are links). The VM test compared the plans of the
Rust program and of `sandbox.sh` for all test sandboxes and modes before
the switch. Still bash: the menus of `nix/sandboxes.nix` and the build-time
scripts (ssh_config, OpenSnitch config). A run with a terminal gets a pty of its own (`src/pty.rs`, decision 5); the
gVisor backend still runs `script` in gVisor, for the signals of the
terminal.

Changes from the plan below:
- `libc` in place of `rustix`, and no `lexopt` (the arguments are simple).
- No control socket: the state directory of the container is only on the
  host; a run checks the PID of a state file with a pidfd and the start
  time, and takes all namespaces from that process. The service is
  `Type=notify`, so the scope of a run starts only when the container is
  ready (no polling). `caller.json` has only the variables that the plan
  reads (in place of the full environment).
- `unshare` is not used: the child makes the user and network namespace and
  writes its maps before exec.

## 1. The code in scope

Runtime shell code (runs each time a sandbox starts or a program runs):

| File | Lines | Runs where | Function |
| ---- | ----: | ---------- | -------- |
| `sandbox.sh` | 1138 | host, as the user | the frontend: config, plan, scopes, network, container service, join, all subcommands |
| `backends/bwrap.sh` | 142 | host (or root of the user namespace of the network) | plan → bwrap arguments; `run`, `start`, `exec` (3× nsenter), `show` |
| `backends/gvisor.sh` | 223 | same | plan → OCI bundle; runsc `run`, `start`, `exec`, `show`; signal forwarding |
| `init.sh` | 83 | **in the sandbox** (PID 2 in bwrap, PID 1 in gVisor) | dbus-daemon, xwayland-satellite, env file, hold/reap, join |
| `module.nix` `commandWrapper` / `commandStart` | ~70 | **in the sandbox** and on the host | the `commands`: refuse outside their sandboxes; `cwdOnly` nested bwrap |
| `module.nix` `yoloClaude` | 9 | in the sandbox | `claude --dangerously-skip-permissions` |
| `nix/sandboxes.nix` `drun`, `terminal`, `hostTerminal` | ~50 | host (kwm bindings) | menus: `sandbox pick`, then rofi or Ghostty |

Build-time shell code (runs once in the Nix build, with no input from a
sandbox): `sshConfig` (ssh_config without `Include` of `/nix/store`) and
`opensnitchConfig` (jq merge). These do not need a migration for security; a
Rust helper can replace them later for uniformity.

## 2. External interfaces that must stay the same

Other programs depend on these. A migration must keep them, or change the
consumers in the same step.

### 2.1 The command line

| Command | Consumers |
| ------- | --------- |
| `sandbox run [--need-cwd] NAME [--] [CMD...]` (default `$SHELL`) | menus (`nix/sandboxes.nix`), `modules/zed.nix`, users |
| `sandbox host [--] [CMD...]` | menus, `hostTerminal`, `river.nix` (scope names) |
| `sandbox current` | every command wrapper, `zed.nix`, the zsh prompt |
| `sandbox pick [--host] [--gui]` | menus |
| `sandbox list [--names\|--json]`, `ps`, `stop NAME`, `show NAME`, `home NAME`, `backends` | users, tests |
| internal: `__container NAME` (ExecStart of `sandbox@.service`), `__container_ns DIR`, `__netns DIR` | module.nix, the frontend itself |

Exit codes: `run` returns the exit code of the program (tests check 7, with
and without a terminal); errors are exit 1 with `sandbox: MESSAGE` on stderr;
usage errors exit 2.

Messages that tests assert (keep the text, or change test.nix):
- command wrappers: `NAME runs only in the sandboxes A, B, not on the host`,
  `... not in the sandbox X`; `cwdOnly`: `the path D of the sandbox S has the
  configuration C, and cwdOnly would hide it.`
- `run --need-cwd`: `does not have the current directory`.

### 2.2 Environment variables

Read by the frontend: `SANDBOX_CONFIG`, `SANDBOX_BACKEND`, `SANDBOX_NO_SCOPE`,
`SANDBOX__SCOPED` (internal), `SANDBOX__CALLER_ENV` (internal),
`INVOCATION_ID` (from systemd), `XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`,
`DISPLAY`, `XAUTHORITY`, `SSH_AUTH_SOCK`, `DOCKER_HOST`, `XDG_CONFIG_HOME`,
`XDG_DATA_HOME`, `XDG_CACHE_HOME`, `HOME`, `PWD`, `PATH`, `SHELL`, and the
`passEnv` names. Backends: `SANDBOX_RUNSC_PLATFORM`, `SANDBOX_RUNSC_NVPROXY`.

Set in the sandbox: `SANDBOX_NAME`, `SANDBOX_COLOR`, `SANDBOX_BACKEND`,
`XDG_RUNTIME_DIR`, `XDG_SESSION_TYPE=wayland`, `WAYLAND_DISPLAY=wayland-0`,
`DISPLAY` and `DBUS_SESSION_BUS_ADDRESS` (from init), `NO_AT_BRIDGE=1`,
`GTK_A11Y=none` (without a host bus), `GTK_THEME`, `PATH` (bins first),
`SSH_AUTH_SOCK`, `DOCKER_HOST`, `NIX_REMOTE=daemon`, the `env` of the
sandbox, and the internal `SANDBOX_INIT_*`.

Set on slirp4netns and the backend: `SANDBOX_NAME`, `SANDBOX_COLOR`. The rule
`allowOnHost` of the host OpenSnitch matches `process.path` = the slirp4netns
of the store **and** `process.env.SANDBOX_NAME`, so the Rust code must start
the same slirp4netns path with that variable.

### 2.3 `/etc/sandboxes/config.json`

Written by Nix (`toJSON`, `configFile` in module.nix), read by the frontend,
**kwm** (`src/kwm/sandbox.zig`: colors), **netgraph.sh**, and the
**OpenSnitch UI fork** (`dialogs/prompt/utils.py`: colors). The schema stays.
Rust reads it with typed structs (serde), and an unknown or wrong field is an
error at start, not a jq `null` at run time.

The command wrappers have their data in the generated bash, not in
config.json. A Rust `sandbox __command NAME` needs a new `commands` object in
config.json (per command: the sandboxes, the program path, `cwdOnly`,
`bwrapArgs`), and per sandbox the targets of `paths` and of `configMounts`
(already there).

### 2.4 systemd units and cgroups

- `sandbox.slice`, `sandbox-NAME.slice` (declared by Nix; `sliceConfig`).
- Run scopes `sandbox-NAME-PID-RANDOM.scope` in `sandbox-NAME.slice`, with
  `Requires=` and `After=sandbox@NAME.service`, description `sandbox NAME:
  CMD` (`sandbox ps` parses the unit name and the description).
- `sandbox@NAME.service` (template in Nix, `StopWhenUnneeded`,
  `Slice=sandbox-%i.slice`), log in its journal.
- Host scopes `app-sandbox-host-PID-RANDOM.scope` in `app.slice`
  (`river.nix` refers to them).
- kwm and the OpenSnitch fork find the sandbox from the cgroup path
  component `sandbox-<name>.slice`; the zed wrapper greps
  `/sandbox-code.slice/` in `/proc/PID/cgroup`.

### 2.5 Files and directories

| Path | Owner | Content |
| ---- | ----- | ------- |
| `stateDir/NAME/home` | frontend | persistent home (0700) |
| `opensnitch.directory` (`stateDir/NAME/opensnitch`) | frontend | `config.json`, `rules/` (store links are replaced at each start, GUI files stay), `opensnitchd.log` |
| `/tmp/sandbox-UID-NAME` | frontend | `sharedTmp` (check: not a link, a directory, owner UID, mode 0700) |
| `$XDG_RUNTIME_DIR/sandboxes/NAME/caller.env` | run → service | the full environment of the run that starts the container |
| `$XDG_RUNTIME_DIR/sandboxes/NAME/c-INVOCATION/` | service | `plan.json`, `backend.path`, `resolv.conf`, `net.sh`, FIFOs `go`/`ready`, `netns` (PID + start time), `up`, `backend/` (`info.json`, `pid`, `ready`, runsc `state/`, bundle) |
| `$XDG_RUNTIME_DIR/sandbox-NAME.XXXXXX/` | run | `plan.json` (`cleanTmp`), `resolv.conf`, `net.sh` |
| in the sandbox: `$XDG_RUNTIME_DIR/{sandbox-init.env,bus,dbus-daemon.log,xwayland-satellite.log}` | init | services of the container |

### 2.6 The backend contract (plan.json)

`<backend> run|start|exec|show PLAN.json`, plan version 1 (README). Keep it
if third-party backends must work; else it becomes an internal type. See the
open questions.

## 3. Behavior inventory

Each item is a behavior that the Rust code must have, with the place in the
bash code today.

### 3.1 Configuration (`load_env`, sandbox.sh:102)

1. Read config.json; unknown sandbox name → error that names `sandbox list`.
2. Expand `~` and remove empty path segments (`expand`): the cwd tests
   compare strings, and `normPath` of module.nix does the same in Nix.
3. Backend: `SANDBOX_BACKEND` overrides; `runsc` is an alias of `gvisor`;
   unknown backend → error.
4. `homeDirectory`, `opensnitch.directory` with `~`.

### 3.2 The plan (`build_plan`, sandbox.sh:195; `make_plan_json`)

Modes: `run` (own container), `container` (the service), `join` (a run in
the container: only command, env, cwd, tty, network, state are used).

1. **Terminal**: `tty` if stdin and stdout are terminals (never in
   `container` mode).
2. **Network**: `disableNetworking` → none; `shareHostNetwork` → host; else
   private.
3. **Environment**: only `passEnv` names from the caller; always remove
   `DISPLAY`, `XAUTHORITY`, `DBUS_SESSION_BUS_ADDRESS`, `WAYLAND_DISPLAY`
   first; the later steps set them. `env`/`extraEnvs` last (they win).
4. **Init**: `container` → `SANDBOX_INIT_HOLD=1`; `join` →
   `SANDBOX_INIT_JOIN=1`; `run` → init only if X11 or D-Bus is private.
5. **Mounts, in this order** (a later mount covers an earlier one):
   `/nix/store` ro, `/etc` ro, the ssh_config copy on
   `/etc/ssh/ssh_config` (if the host has the file), `/bin`, `/usr`,
   `/run/current-system`, `/run/opengl-driver(-32)` ro; resolv.conf (host:
   the target of the link; private: the file of the run on the target of the
   `/etc/resolv.conf` link); `/tmp` (`mountTmp` → host `/tmp`; `sharedTmp`
   → checked directory; else tmpfs 1777); `$XDG_RUNTIME_DIR` tmpfs 0700;
   home (`allowHome` → real home rw; persistent → mkdir + chmod 0700 + bind;
   else tmpfs 0700); `homePatterns` (`rw:`/`ro:` prefix, from the host
   home); `files` (ro, store paths, on `$HOME/<path>`); gui (Wayland socket
   of `$WAYLAND_DISPLAY`, absolute or relative to the runtime dir, on
   `wayland-0`; warn if missing; `/dev/dri` dev; `/sys` ro); x11 host
   (`/tmp/.X11-unix` ro, `DISPLAY`, `XAUTHORITY` file ro); audio
   (`pipewire-0`, `pulse`, `/dev/snd`); D-Bus host (`$run/bus`); the
   program configs (`configMounts`: XDG remap, `host_path` for
   `.config`/`.local/share`/`.cache`, mkdir of a missing rw directory); ssh
   agent socket; gpg (`~/.gnupg` rw, the socket dir of `gpgconf
   --list-dirs socketdir`, fallback `$run/gnupg`); docker (the first socket of
   `DOCKER_HOST`, `/run/docker.sock`, `/var/run/docker.sock`,
   `$run/docker.sock`); fuse, kvm (+ vfio), nvidia (every `/dev/nvidia*`,
   `/sys`), libvirt, nix daemon socket; home caches; common home folders;
   `paths` (`ro:`/`rw:`, `src:dst`, warn and skip if missing).
   A bind of a missing source is skipped without a message (`bind`).
6. **Same paths** (source = target) give the working directory: the
   program starts in `$PWD` if it is below one of them (or one is `/`), else
   in `$HOME`. `--need-cwd` → error if not.
7. X11 private on the host network: random display 100–899 (abstract
   sockets are in the network namespace).
8. Plan JSON: version 1, name, hostname, uid, gid, home, cwd, tty, network
   (`none`/`inherit`), state, mounts, env, command, `backendArgs[backend]`.

### 3.3 Network (sandbox.sh:510–760)

1. `allowHosts` resolved on the host (`getent ahosts`), warn if a name does
   not resolve.
2. `socksProxy`: `HOST:PORT` means `socks5://`; the proxy host gets a route
   via `10.0.2.2` and is allowed by the firewall; tun0 `198.18.0.1/15`,
   default route on tun0, tun2socks.
3. New namespace: `unshare --user --map-root-user --net`; wait until
   `/proc/PID/ns/net` differs (5 s); slirp4netns `--configure --mtu=65520
   [--disable-host-loopback] --ready-fd` (5 s, warn on timeout); a FIFO
   `go` releases the process in the namespace.
4. In the namespace: `lo` up; `ping_group_range = 0 0`; firewall:
   `internetAccess=false` → only private nets + allowed IPs, policy DROP,
   v4 and v6; `lanAccess=false` → reject private nets, keep `10.0.2.3`
   (DNS of slirp4netns).
5. `privacyFilter` → `socks5://10.0.2.2:1080` and host loopback (Nix side).
6. OpenSnitch (`opensnitch_start`): rules dir (delete store links, link the
   current rules), config (`Server.Name`, `Server.LogFile`, `Rules.Path`),
   start the daemon, **wait until nft `inet opensnitch mangle_output` has a
   queue rule** (20 s), so no connection leaves before the daemon sees it;
   the container process restarts a stopped daemon (1 s later), and stops
   the container if that fails.
7. A run without a scope with OpenSnitch: its own daemon, log in
   `opensnitchd.log`.

### 3.4 Container service (`cmd_container`, `container_ns`, sandbox.sh:672, 765)

1. Load `caller.env` (drop `BASH_FUNC_*`, `PWD`, `OLDPWD`, `SHLVL`, `_`,
   `SANDBOX__*`), re-exec with exactly that environment plus
   `INVOCATION_ID`; refuse to run without `INVOCATION_ID`.
2. `cd $HOME`, remove old `c-*`, make `c-INVOCATION` 0700, clean up on exit,
   TERM/INT → exit 0.
3. Plan in `container` mode; write `backend.path`.
4. Private network: the namespace process sets up the network, OpenSnitch,
   starts `<backend> start`, waits for the backend or the daemon (restart
   loop). Else: `<backend> start` directly.
5. Wait for `backend/ready` (30 s, fail if a process stops), write `netns`
   (PID + start time) and `up`; stop when any child stops (exit 1 with a
   message).

### 3.5 Run (`cmd_run`, `join_container`, `run_joined`, sandbox.sh:840–945)

1. With a user manager and no `SANDBOX_NO_SCOPE`: write `caller.env`
   atomically (dirs 0700), `exec systemd-run --user --scope --collect` with
   the slice, unit name, description, `Requires`/`After`, and
   `SANDBOX__SCOPED=1`.
2. In the scope: find the `InvocationID` of `sandbox@NAME`, wait for
   `c-ID/up` (30 s; fail if the service is not active), plan in `join` mode
   with the resolv.conf and state of the container.
3. **Mount check**: compare the mounts of the run with the mounts of the
   container; warn with the targets that differ.
4. Enter the network: verify PID + start time of `netns`, then `nsenter
   --user --net --preserve-credentials`; then `<backend> exec`.
5. Without a scope: own container (`run` mode, private network or not).
6. `trap : INT` (Ctrl-C goes to the program), remove the run directory if
   `cleanTmp`, exit with the code of the program.

### 3.6 bwrap backend (`backends/bwrap.sh`)

1. Arguments: `--die-with-parent`, unshare user/ipc/pid/uts/cgroup-try,
   uid/gid, hostname, `--new-session` without a terminal, `--unshare-net` for
   none, `--proc`, `--dev`, `--tmpfs /dev/shm`, the mounts (bind/ro-bind,
   dev-bind, perms + tmpfs), `--clearenv`, setenv, chdir, extra args.
2. `start`: `--info-fd` to a file; poll the JSON for `child-pid`; **ready
   only when that PID has a child** (bwrap writes the PID before its
   `pivot_root`; before that, the root of that process is the root of the
   host); write `pid` (PID + start time), `ready`; wait.
3. `exec`: check PID + start time; 1st `nsenter --user-parent`, 2nd
   `nsenter --mount --pid --ipc --uts --cgroup [--net] --root --wdns=CWD`,
   3rd `nsenter --target 1 --user` (the PID in the `/proc` of the
   container), then `setpriv --no-new-privs --pdeathsig KILL`, `setsid
   --wait` without a terminal, with `env -i` + the env of the plan.

### 3.7 gVisor backend (`backends/gvisor.sh`)

1. Devices are skipped with a warning; terminal → the command runs in
   `script` behind `sh -c '"$@"; exit $?'`.
2. OCI config: process (uid 0, no capabilities, `noNewPrivileges`, NOFILE
   65536), read-only empty rootfs, proc/dev/shm/pts/sysfs, the mounts
   (`rbind`, `rprivate`, `ro`; no `/sys` from the plan), namespaces
   pid/ipc/uts/mount.
3. Flags: `--root`, `--rootless`, `--ignore-cgroups`, `--platform`
   (systrap), `--overlay2=root:memory`, `--host-uds=open`, network
   none/host, `--nvproxy`, extra args.
4. `run`/`start`: TERM/HUP → `runsc kill --all SIGKILL`; WINCH → `kill
   --all SIGWINCH` with a terminal; wait again after a signal. `start` waits
   for `runsc state` = running (30 s), then `ready`.
5. `exec`: `runsc exec --process process.json --internal-pid-file`; TERM/HUP
   kill that PID only.

### 3.8 Init in the sandbox (`init.sh`)

1. Join: wait for `sandbox-init.env` (10 s), source it, exec the command.
2. Private D-Bus: `dbus-daemon --fork` with the session.conf of the package.
3. Private X11: first free display (`/tmp/.X11-unix/XN`, `/tmp/.XN-lock`),
   xwayland-satellite, wait for the socket (5 s), `DISPLAY`.
4. Hold: write the env file atomically, reap children forever (PID 1 in
   gVisor).

### 3.9 Other subcommands

`show` (plan + backend command, nothing starts), `list` (table with color
swatch on a terminal, network summary, capability summary; `--names`,
`--json`), `pick` (menu command of the config via `sh -c`, Pango markup,
escape `&<>`, strip tags, first word, host entry), `ps` (running scopes
`sandbox-*`: name, MainPID, command), `stop` (stop the slice), `host` (scope
in `app.slice`, `SANDBOX_NAME=host`, host color, unset `SANDBOX_BACKEND` and
`SANDBOX__SCOPED`), `current` (`SANDBOX_NAME`, but `host` if
`/proc/1/comm` is systemd), `home`, `backends`.

### 3.10 Commands (module.nix `commandWrapper`, `commandStart`)

1. `sandbox current` decides: in a sandbox with the command, start it; else
   the error text of 2.1.
2. `cwdOnly`/`bwrapArgs`: nested `bwrap --dev-bind / /`; each `paths`
   target: directory → tmpfs, file → `/dev/null` ro; then bind `$PWD`; refuse
   if a path covers a config mount (or is `/`); `--die-with-parent
   --chdir $PWD`.
3. `yolo`: a `claude` first in PATH that removes its own directory from PATH
   and adds `--dangerously-skip-permissions`.

## 4. What Rust changes for security

The threat is a program in a sandbox. The host side runs as the same user,
so the code that matters is the code that reads something a sandbox can
change, or that holds capabilities while it enters a sandbox.

### 4.1 Real gains

- **No shell parsing of data.** Today: `eval` of jq `@sh` output
  (`load_env`), `source` of `net.sh` (printf `%q`), word lists through jq NUL
  strings, `$OSN_PID $T2S` unquoted. Each one is correct now, but a later
  edit can break the quoting. Typed values remove this class of bugs.
- **No PATH lookups on the host.** `package.nix` has `inheritPath = true`:
  `systemctl`, `systemd-run`, `gpgconf` come from the PATH of the caller.
  Rust can use absolute store paths (or D-Bus for systemd).
- **pidfds instead of PID files.** The join checks PID + start time today
  (a small race). The service can hold pidfds and namespace fds, and give
  them to a run on a unix socket (`SCM_RIGHTS`, peer uid check with
  `SO_PEERCRED`). Then there is no PID file, no start-time check, no
  `pgrep`, no polling of `info.json`.
- **setns in one process.** The three `nsenter` plus `setpriv` and `setsid`
  become one sequence: `setns(parent userns)`, `setns(mnt, pid, ipc, uts,
  cgroup[, net])`, `fchdir`/`chroot` to the root fd of the container,
  `setns(userns)`, fork, `prctl(NO_NEW_PRIVS)`, `PDEATHSIG`, `setsid`,
  `execve` with an explicit envp. Fewer programs run with capabilities in
  the user namespace of the container.
- **Readiness without polling.** `Type=notify` with `sd_notify(READY=1)`:
  the scope of a run has `After=sandbox@NAME`, so systemd starts the run
  only when the container is ready. This removes `join_container` and the
  `up` file.
- **No caller environment on disk.** `caller.env` has every variable of the
  caller (tokens too) in `$XDG_RUNTIME_DIR` (tmpfs, 0700, host only). The
  run can send only the variables the plan uses, on the control socket.
- **Typed config.** serde structs for config.json: a schema error stops at
  start with a clear message.
- **Unit tests** of the pure parts: path expansion, the plan, the mount
  order, cwd selection, the comparison of mounts, `/proc/PID/stat` parsing.

### 4.2 No change (the isolation is the kernel and bwrap/runsc)

- The sandbox has the full Wayland socket (screen capture, all protocols);
  `dbus=host`, `x11=host` and `docker` remove the isolation, as documented.
- Same uid on the host: the protection against ptrace of host processes is
  the user namespace and the PID namespace. During `exec`, the process that
  enters has capabilities in the parent user namespace; a process of the
  sandbox (in the child namespace) cannot ptrace it (commoncap:
  `ns_capable` in the parent fails). Rust keeps this property; the window is
  shorter.
- TIOCSTI: a run with a terminal shares the terminal of the caller.
  `dev.tty.legacy_tiocsti` is 0 on this kernel (6.18), so TIOCSTI fails. The
  module should set that sysctl, or the run should give the program a pty of
  its own (as `script` does for gVisor). That is a feature for either
  language.

### 4.3 Points to check during the migration (exist today)

- `files` and `configMounts` mount on targets below the writable home of
  the sandbox. A program of the sandbox can put a symlink at such a target
  before the next start. Check how bwrap resolves a symlink target in
  `/newroot` (bwrap uses `O_PATH` and checks, but test it), and refuse a
  symlink target in the Rust code before bwrap sees it.
- The `sharedTmp` checks (not a link, owner, mode) and the bind are two
  steps. With an fd (`O_PATH|O_NOFOLLOW`, `fstat`, bind of `/proc/self/fd/N`
  or `open_tree`), there is one object.
- The mount check only warns. Rust can refuse, or ask.

## 5. Proposed structure

One Rust crate, one multicall binary `sandbox` (no bash at run time):

| Module | Replaces |
| ------ | -------- |
| `config` (serde) | `load_env`, jq queries |
| `plan` (pure: config + caller env + cwd + mode → `Plan`) | `build_plan`, `make_plan_json` |
| `scope` (systemd-run with absolute path, or D-Bus `StartTransientUnit`) | `exec systemd-run` |
| `net` (unshare/uid_map, netlink for lo/tun/routes, iptables or nft, slirp4netns and tun2socks as children) | `netns_*`, `write_net_settings` |
| `opensnitch` (rules dir, config, start, nft readiness, restart) | `opensnitch_start` |
| `container` (service: `Type=notify`, control socket, fds for runs) | `cmd_container`, `container_ns`, `join_container` |
| `backend::bwrap`, `backend::gvisor` | `backends/*.sh` |
| `join` (setns sequence in process) | 3× nsenter, setpriv, setsid |
| `init` (`sandbox __init`, static or store binary in the sandbox) | `init.sh` |
| `command` (`sandbox __command NAME`; wrappers are links or `makeBinaryWrapper`) | `commandWrapper`, `commandStart`, `yoloClaude` |
| `cli` (`list`, `pick`, `ps`, `stop`, `host`, `current`, `home`, `backends`, `show`) | the `cmd_*` functions |

External programs that stay: `bwrap`, `runsc`, `slirp4netns`, `tun2socks`,
`opensnitchd`, `dbus-daemon`, `xwayland-satellite`, `gpgconf`, the menu
command; and probably `iptables`/`nft` (a netlink nftables crate is
possible, but it is more code to trust).

Crates: `serde`/`serde_json`, `rustix` or `nix` (namespaces, mount, pidfd,
prctl, SCM_RIGHTS), optional `zbus` (systemd), optional `rtnetlink`. Keep the
list short: each crate is code that runs with capabilities in the
namespaces.

## 6. Migration steps

Each step keeps `checks.x86_64-linux.sandboxes` green; that VM test is the
acceptance test (it tests the CLI from outside).

1. **Plan parity.** Rust `plan` + `sandbox show --json`. Add a VM subtest
   that compares the bash plan and the Rust plan of every sandbox of the
   test (both modes). No behavior change.
2. **Frontend.** Rust `run` (with the bash backends and init), `host`,
   `current`, `list`, `pick`, `ps`, `stop`, `home`, `backends`, `show`.
3. **Container.** Service with `Type=notify` and the control socket; join
   with fds; remove `caller.env`, `c-*/up`, PID files.
4. **Backends.** bwrap and gVisor in Rust; the join without nsenter.
5. **Init.** `sandbox __init` in the sandbox.
6. **Commands.** `commands` in config.json; `sandbox __command`; remove the
   generated bash and `yoloClaude`.
7. **Remove** `sandbox.sh`, `init.sh`, `backends/*.sh`; update README,
   module docs, CLAUDE.md. The menus in `nix/sandboxes.nix` can stay (small,
   host data only) or become `sandbox menu drun|terminal`.

Size: about 1600 lines of runtime bash today; a Rust version with tests is
probably 3000–4500 lines.

## 7. Decisions

1. **Backends**: the external contract (plan.json, `run|start|exec|show`)
   stays, for the backends of other people. bwrap and gVisor are built into
   the binary, and they implement the same contract.
2. **systemd**: `systemd-run` and `systemctl` with absolute paths, not D-Bus.
   Nix writes the path of `config.systemd.package` into config.json, so the
   commands agree with the systemd that runs, and the PATH of the caller is
   not used. D-Bus would need a D-Bus crate (and often an async runtime) and
   the JobRemoved handshake for a transient scope.
3. **Firewall**: the `iptables`, `ip6tables`, `nft` and `ip` programs, with
   absolute store paths and one argument per `Command::arg` (no shell). The
   rules are only in the network namespace of the sandbox; a netlink
   nftables crate would add more code with capabilities in that namespace.
4. **Mount mismatch**: open. Each rebuild changes store paths (`files`,
   ssh_config), thus a running container differs from a new configuration
   after almost every rebuild.
   - warn: runs keep working; a smaller access (a path removed from
     `paths`) applies only after `sandbox stop`;
   - refuse: no new program in a running sandbox until `sandbox stop`, which
     stops its other programs; the running programs keep the old access; a
     different `SSH_AUTH_SOCK` of the caller also refuses;
   - proposal: refuse when the access becomes smaller (a mount that the new
     plan does not have, rw that becomes ro), warn otherwise; `sandbox ps`
     marks a stale container.
5. **Terminal**: a run with a terminal gets a pty of its own; the frontend
   copies the bytes and forwards SIGWINCH. Same behavior for bwrap and
   gVisor, no `script` in gVisor, no TIOCSTI on the terminal of the caller.
   Ctrl-Z stops the program in the sandbox, not `sandbox run` (as `ssh`).
   A step after the parity of the plan. Until then, the module sets
   `dev.tty.legacy_tiocsti = 0` (mkDefault).
6. **Crates**: `serde`, `serde_json`, `rustix`, `lexopt`. No async runtime,
   no D-Bus crate, no netlink crate. Built with nixpkgs `rustPlatform`,
   Cargo.lock in the repo.
