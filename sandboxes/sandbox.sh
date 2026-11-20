# sandbox: start programs in the named sandboxes of programs.sandboxes.
#
# The NixOS module writes the sandboxes to /etc/sandboxes/config.json. The
# SANDBOX_CONFIG environment variable gives a different file.
#
# This command does not isolate a program itself. It makes a plan: a JSON file
# with the mounts, the environment, the network and the command. A backend
# (bwrap, gvisor, ...) reads the plan and starts the program. Refer to
# README.md for the format of the plan.
#
# This command also gives the parts that all backends share:
#
#   - a systemd scope below sandbox-NAME.slice (sandbox ps, sandbox stop),
#   - the network namespace with slirp4netns and the firewall,
#   - the OpenSnitch daemon in the network namespace (opensnitch.enable).

set -euo pipefail

CONFIG=${SANDBOX_CONFIG:-/etc/sandboxes/config.json}
SELF=$(readlink -f "${BASH_SOURCE[0]}")

die() {
  printf 'sandbox: %s\n' "$*" >&2
  exit 1
}

warn() { printf 'sandbox: %s\n' "$*" >&2; }

usage() {
  cat <<'EOF'
Usage: sandbox COMMAND [ARGUMENTS]

  list [--names|--json]   Show the sandboxes.
  show NAME               Show the plan and the backend command of NAME.
  run [--need-cwd] NAME [--] [CMD...]
                          Run CMD in the sandbox NAME. The default is $SHELL.
                          CMD starts in the current directory if the sandbox
                          has it, else in the home directory. With
                          --need-cwd, the command stops if the sandbox does
                          not have the current directory.
  pick [--host] [--gui]   Select a sandbox in the menu and print its name.
                          With --host, the menu also shows "host". With --gui,
                          the menu shows only the sandboxes with gui.
  ps                      Show the programs that run in sandboxes.
  stop NAME               Stop all programs of the sandbox NAME.
  host [--] [CMD...]      Run CMD on the host, not in a sandbox, with the label
                          of the host (SANDBOX_NAME=host), in its own scope
                          below app.slice. The default is $SHELL.
  current                 Print the sandbox of this process, or "host".
                          SANDBOX_NAME gives the sandbox. On a host where a
                          program set SANDBOX_NAME, PID 1 is systemd: then
                          the command prints "host".
  home NAME               Print the home directory of NAME on the host.
  backends                Show the backends.

Environment:
  SANDBOX_CONFIG          The configuration file (default /etc/sandboxes/config.json).
  SANDBOX_BACKEND         Use this backend in place of the backend of the sandbox.
  SANDBOX_NO_SCOPE=1      Do not start a systemd scope.
EOF
}

# Expand a "~" at the start of a path to $HOME, and drop the empty segments:
# "~/code/" and "~//code" are $HOME/code. The tests of the current directory
# compare the strings.
expand() {
  local p
  case $1 in
    \~) p=$HOME ;;
    \~/*) p=$HOME/${1#\~/} ;;
    *) p=$1 ;;
  esac
  while [[ $p == *//* ]]; do p=${p//\/\//\/}; done
  [ "$p" = / ] || p=${p%/}
  printf '%s\n' "$p"
}

# The path on the host of the path $1 below $HOME. A path below .config,
# .local/share or .cache is in the XDG directory of the host.
host_path() {
  case $1 in
    .config/*) printf '%s\n' "${XDG_CONFIG_HOME:-$HOME/.config}/${1#.config/}" ;;
    .local/share/*) printf '%s\n' "${XDG_DATA_HOME:-$HOME/.local/share}/${1#.local/share/}" ;;
    .cache/*) printf '%s\n' "${XDG_CACHE_HOME:-$HOME/.cache}/${1#.cache/}" ;;
    *) printf '%s\n' "$HOME/$1" ;;
  esac
}

require_config() {
  [ -r "$CONFIG" ] || die "cannot read $CONFIG"
}

names() { jq -r '.environments | keys[]' "$CONFIG"; }

has_env() { jq -e --arg n "$1" '.environments | has($n)' "$CONFIG" >/dev/null; }

# Read all values of the sandbox $1 into E_* shell variables with one jq call.
load_env() {
  has_env "$1" || die "no sandbox has the name '$1'. Run 'sandbox list'."
  NAME=$1
  local code
  code=$(jq -r --arg n "$1" '
    .environments[$n] as $e
    | ($e.files | to_entries | map(.key, .value)) as $files
    | ($e.env | to_entries | map("\(.key)=\(.value)")) as $env
    | @sh "
      E_DESCRIPTION=\($e.description)
      E_COLOR=\($e.color)
      E_BACKEND=\($e.backend)
      E_GTK_THEME=\($e.gtkTheme // "")
      E_HOSTNAME=\($e.hostname)
      E_HOME_DIR=\($e.homeDirectory)
      E_PERSISTENT_HOME=\($e.persistentHome)
      E_ALLOW_HOME=\($e.allowHome)
      E_SSH=\($e.ssh) E_GPG=\($e.gpg)
      E_DOCKER=\($e.docker) E_FUSE=\($e.fuse) E_GUI=\($e.gui) E_X11=\($e.x11)
      E_DBUS=\($e.dbus) E_NVIDIA=\($e.nvidia) E_KVM=\($e.kvm) E_AUDIO=\($e.audio)
      E_LIBVIRT=\($e.libvirt) E_NIX=\($e.nix)
      E_INTERNET=\($e.internetAccess) E_LAN=\($e.lanAccess)
      E_DISABLE_NET=\($e.disableNetworking) E_HOST_NET=\($e.shareHostNetwork)
      E_HOST_LOOPBACK=\($e.allowHostLoopback)
      E_SOCKS=\($e.socksProxy // "")
      E_HOME_CACHE=\($e.mountHomeCache) E_COMMON_HOME=\($e.mountCommonHomeFolders)
      E_MOUNT_TMP=\($e.mountTmp) E_SHARED_TMP=\($e.sharedTmp) E_CLEAN_TMP=\($e.cleanTmp)
      E_OPENSNITCH=\($e.opensnitch.enable)
      E_OSN_DIR=\($e.opensnitch.directory)
      E_OSN_RULES=\($e.opensnitch.rules // "")
      E_OSN_DAEMON=\(.opensnitch.daemon // "")
      E_OSN_CONFIG=\(.opensnitch.config // "")
      E_ALLOW_HOSTS=(\($e.allowHosts))
      E_HOME_PATTERNS=(\($e.homePatterns))
      E_PATHS=(\($e.paths))
      E_CONFIG=(\($e.configMounts | map(.mode, .path, .remap)))
      E_PASS_ENV=(\($e.passEnv))
      E_ENV=(\($env))
      E_FILES=(\($files))
      E_BIN=(\($e.bin))
      E_INIT=\(.init)
      E_SSH_CLIENT_CONFIG=\(.sshConfig // "")
      E_HOME_CACHE_DIRS=(\(.homeCacheDirs))
      E_COMMON_HOME_DIRS=(\(.commonHomeFolders))
    "' "$CONFIG")
  eval "$code"
  E_HOME_DIR=$(expand "$E_HOME_DIR")
  E_OSN_DIR=$(expand "$E_OSN_DIR")
  E_BACKEND=${SANDBOX_BACKEND:-$E_BACKEND}
  BACKEND=$(jq -r --arg b "$E_BACKEND" '.backends[$b] // empty' "$CONFIG")
  # "runsc" is the name of gVisor in sandboxed-agents.
  if [ -z "$BACKEND" ] && [ "$E_BACKEND" = runsc ]; then
    E_BACKEND=gvisor
    BACKEND=$(jq -r '.backends.gvisor // empty' "$CONFIG")
  fi
  [ -n "$BACKEND" ] || die "no backend has the name '$E_BACKEND'. Run 'sandbox backends'."
}

# The plan, in shell variables. make_plan_json writes it as JSON.
M=()         # the mounts: 4 values for each mount (type, source, target, mode)
SAME_PATHS=() # the paths that are at the same place in the sandbox and on the host
declare -A ENVS=()

mnt() { M+=("$1" "$2" "$3" "$4"); }

bind() { # bind MODE SOURCE [TARGET]: MODE is ro, rw or dev.
  local mode=$1 src=$2 dst=${3:-$2}
  [ -e "$src" ] || return 0
  if [ "$mode" = dev ]; then
    mnt dev "$src" "$dst" rw
  else
    mnt bind "$src" "$dst" "$mode"
  fi
  [ "$src" = "$dst" ] && SAME_PATHS+=("$dst")
  return 0
}

tmpfs() { mnt tmpfs "" "$1" "${2:-0755}"; }

setenv() { ENVS[$1]=$2; }

# Make the plan of the sandbox NAME for the command "$@". NET is the network
# of the frontend: none, host, private (a network namespace for this run) or
# shared (the network namespace of the service sandbox-net@NAME).
build_plan() {
  local uid run
  uid=$(id -u)
  run=${XDG_RUNTIME_DIR:-/run/user/$uid}

  # The program has a terminal if this command has one. make_plan_json cannot
  # test it: its standard output is the plan file.
  TTY=false
  if [ -t 0 ] && [ -t 1 ]; then TTY=true; fi

  if [ "$E_DISABLE_NET" = true ]; then
    NET=none
  elif [ "$E_HOST_NET" = true ]; then
    NET=host
  elif [ "$E_OPENSNITCH" = true ] && [ -n "${SANDBOX__SCOPED:-}" ]; then
    # One OpenSnitch daemon for the sandbox: all runs share one network
    # namespace. A run without a scope has no service, and it gets a network
    # namespace and a daemon of its own.
    NET=shared
  else
    NET=private
  fi

  # The environment: only the names of passEnv go in. The blocks below can
  # replace a value.
  local v
  for v in "${E_PASS_ENV[@]}"; do
    [ -n "${!v+x}" ] && setenv "$v" "${!v}"
  done
  unset 'ENVS[DISPLAY]' 'ENVS[XAUTHORITY]' 'ENVS[DBUS_SESSION_BUS_ADDRESS]' 'ENVS[WAYLAND_DISPLAY]'

  # sandbox-init starts the private services before the command.
  INIT=()

  # The base system, read-only.
  bind ro /nix/store
  bind ro /etc
  # ssh_config without the Include of files of /nix/store: ssh refuses them,
  # because their owner is nobody in the sandbox. Refer to module.nix. On
  # NixOS, /etc/ssh/ssh_config is a symbolic link, and a backend cannot mount
  # on a link: the mount goes on the file at the end of the link.
  if [ -n "$E_SSH_CLIENT_CONFIG" ] && [ -e /etc/ssh/ssh_config ]; then
    bind ro "$E_SSH_CLIENT_CONFIG" "$(readlink -f /etc/ssh/ssh_config)"
  fi
  bind ro /bin
  bind ro /usr
  bind ro /run/current-system
  bind ro /run/opengl-driver
  bind ro /run/opengl-driver-32

  # The name server. /etc/resolv.conf can be a link into /run.
  local resolv
  resolv=$(readlink -f /etc/resolv.conf || echo /etc/resolv.conf)
  case $NET in
    host) [ "$resolv" = /etc/resolv.conf ] || bind ro "$resolv" ;;
    private | shared) mnt bind "$TMP/resolv.conf" "$resolv" ro ;;
  esac

  if [ "$E_MOUNT_TMP" = true ]; then
    bind rw /tmp
  elif [ "$E_SHARED_TMP" = true ]; then
    # One /tmp for all runs of the sandbox. A different user can make this
    # path first, so make sure that it is a directory of this user.
    local shared=/tmp/sandbox-$uid-$NAME
    mkdir -m 0700 "$shared" 2>/dev/null || true
    if [ -L "$shared" ] || [ ! -d "$shared" ] || [ "$(stat -c %u "$shared")" != "$uid" ]; then
      die "$shared is not a directory of this user"
    fi
    chmod 0700 "$shared"
    mnt bind "$shared" /tmp rw
  else
    tmpfs /tmp 1777
  fi
  tmpfs "$run" 0700

  # The home directory.
  if [ "$E_ALLOW_HOME" = true ]; then
    bind rw "$HOME"
  elif [ "$E_PERSISTENT_HOME" = true ]; then
    mkdir -p "$E_HOME_DIR"
    chmod 0700 "$E_HOME_DIR"
    mnt bind "$E_HOME_DIR" "$HOME" rw
  else
    tmpfs "$HOME" 0700
  fi

  local p mode
  for p in "${E_HOME_PATTERNS[@]}"; do
    mode=ro
    case $p in rw:*) mode=rw p=${p#rw:} ;; ro:*) p=${p#ro:} ;; esac
    bind "$mode" "$HOME/$p"
  done

  local i
  for ((i = 0; i < ${#E_FILES[@]}; i += 2)); do
    mnt bind "${E_FILES[i + 1]}" "$HOME/${E_FILES[i]}" ro
  done

  # The capabilities.
  if [ "$E_GUI" = true ]; then
    local wl=${WAYLAND_DISPLAY:-wayland-0}
    [[ $wl == /* ]] || wl=$run/$wl
    if [ -S "$wl" ]; then
      bind rw "$wl" "$run/wayland-0"
      setenv WAYLAND_DISPLAY wayland-0
    else
      warn "no Wayland socket at $wl"
    fi
    bind dev /dev/dri
    bind ro /sys
    setenv XDG_SESSION_TYPE wayland
  fi

  # X11. "private": an X server for this run (xwayland-satellite), so an X11
  # client sees no X11 client of the host or of other sandboxes. "host": the
  # X server of the host, without isolation.
  if [ "$E_X11" = private ] && [ "$E_GUI" = true ]; then
    setenv SANDBOX_INIT_X11 1
    # The abstract X11 sockets are in the network namespace. In the namespace
    # of the host, or in a namespace that other runs share, use a display
    # number that the others probably do not use.
    if [ "$NET" = host ] || [ "$NET" = shared ]; then
      setenv SANDBOX_INIT_X11_DISPLAY $((100 + RANDOM % 800))
    fi
    INIT=("$E_INIT")
  elif [ "$E_X11" = host ]; then
    bind ro /tmp/.X11-unix
    [ -n "${DISPLAY:-}" ] && setenv DISPLAY "$DISPLAY"
    if [ -n "${XAUTHORITY:-}" ]; then
      bind ro "$XAUTHORITY"
      setenv XAUTHORITY "$XAUTHORITY"
    fi
  fi

  if [ "$E_AUDIO" = true ]; then
    bind rw "$run/pipewire-0"
    bind rw "$run/pulse"
    bind dev /dev/snd
  fi

  # D-Bus. "private": a session bus for this run. "host": the session bus of
  # the host, without isolation.
  if [ "$E_DBUS" = host ]; then
    bind rw "$run/bus"
    setenv DBUS_SESSION_BUS_ADDRESS "unix:path=$run/bus"
  else
    if [ "$E_DBUS" = private ]; then
      setenv SANDBOX_INIT_DBUS 1
      INIT=("$E_INIT")
    fi
    # There is no accessibility bus, and GTK must not look for it.
    setenv NO_AT_BRIDGE 1
    setenv GTK_A11Y none
  fi

  # The configuration of the programs (ssh, git, claude, ...): MODE PATH
  # REMAP for each path below $HOME. With xdgRemap, REMAP is the XDG
  # directory of the program, and it replaces PATH if it is on the host.
  local src dst remap
  for ((i = 0; i < ${#E_CONFIG[@]}; i += 3)); do
    mode=${E_CONFIG[i]} p=${E_CONFIG[i + 1]} remap=${E_CONFIG[i + 2]}
    src=$(host_path "$p")
    if [ -n "$remap" ] && [ -e "$(host_path "$remap")" ]; then
      src=$(host_path "$remap")
    fi
    # A writable directory that is not on the host: make it, so that the
    # files of the program go to the host and not to the sandbox home.
    if [ "$mode" = rw ] && [[ $p == */ ]]; then
      mkdir -p "$src"
    fi
    bind "$mode" "${src%/}" "$HOME/${p%/}"
  done

  if [ "$E_SSH" != none ]; then
    if [ -n "${SSH_AUTH_SOCK:-}" ] && [ -S "$SSH_AUTH_SOCK" ]; then
      bind rw "$SSH_AUTH_SOCK"
      setenv SSH_AUTH_SOCK "$SSH_AUTH_SOCK"
    fi
  fi

  if [ "$E_GPG" = true ]; then
    bind rw "$HOME/.gnupg"
    local sockdir
    sockdir=$(gpgconf --list-dirs socketdir 2>/dev/null || echo "$run/gnupg")
    bind rw "$sockdir"
  fi

  if [ "$E_DOCKER" = true ]; then
    local sock="" s
    for s in "${DOCKER_HOST#unix://}" /run/docker.sock /var/run/docker.sock "$run/docker.sock"; do
      if [ -n "$s" ] && [ -S "$s" ]; then
        sock=$s
        break
      fi
    done
    if [ -n "$sock" ]; then
      bind rw "$sock"
      setenv DOCKER_HOST "unix://$sock"
    else
      warn "docker: no Docker socket found"
    fi
  fi

  [ "$E_FUSE" = true ] && bind dev /dev/fuse
  if [ "$E_KVM" = true ]; then
    bind dev /dev/kvm
    bind dev /dev/vfio
  fi
  if [ "$E_NVIDIA" = true ]; then
    local d
    for d in /dev/nvidia*; do bind dev "$d"; done
    bind ro /sys
  fi
  if [ "$E_LIBVIRT" = true ]; then
    bind rw /run/libvirt
    bind rw "$run/libvirt"
  fi
  if [ "$E_NIX" = true ]; then
    bind rw /nix/var/nix/daemon-socket
    setenv NIX_REMOTE daemon
  fi

  if [ "$E_HOME_CACHE" = true ]; then
    for p in "${E_HOME_CACHE_DIRS[@]}"; do bind rw "$HOME/$p"; done
  fi
  if [ "$E_COMMON_HOME" = true ]; then
    for p in "${E_COMMON_HOME_DIRS[@]}"; do bind rw "$HOME/$p"; done
  fi

  # The paths of the configuration: "/p", "ro:/p", "/host:/guest", "ro:/host:/guest".
  for p in "${E_PATHS[@]}"; do
    mode=rw
    case $p in ro:*) mode=ro p=${p#ro:} ;; rw:*) p=${p#rw:} ;; esac
    if [[ $p == *:* ]]; then
      src=$(expand "${p%%:*}")
      dst=$(expand "${p#*:}")
    else
      src=$(expand "$p")
      dst=$src
    fi
    if [ -e "$src" ]; then
      bind "$mode" "$src" "$dst"
    else
      warn "$src does not exist; the sandbox does not get it"
    fi
  done

  setenv XDG_RUNTIME_DIR "$run"
  setenv SANDBOX_NAME "$NAME"
  setenv SANDBOX_COLOR "$E_COLOR"
  setenv SANDBOX_BACKEND "$E_BACKEND"
  [ -n "$E_GTK_THEME" ] && setenv GTK_THEME "$E_GTK_THEME"
  local path=$PATH
  for p in "${E_BIN[@]}"; do path=$p:$path; done
  setenv PATH "$path"
  for v in "${E_ENV[@]}"; do setenv "${v%%=*}" "${v#*=}"; done

  # The program starts in the current directory if the sandbox has it.
  CWD=$HOME HAS_CWD=false
  for p in "${SAME_PATHS[@]}"; do
    if [ "$p" = / ] || [ "$PWD" = "$p" ] || [[ $PWD == "$p"/* ]]; then
      CWD=$PWD HAS_CWD=true
      break
    fi
  done
}

# Write the plan to stdout. Refer to README.md for the format.
make_plan_json() {
  local envs=() k
  for k in "${!ENVS[@]}"; do envs+=("$k" "${ENVS[$k]}"); done
  jq -n \
    --arg name "$NAME" \
    --arg hostname "$E_HOSTNAME" \
    --argjson uid "$(id -u)" \
    --argjson gid "$(id -g)" \
    --arg home "$HOME" \
    --arg cwd "$CWD" \
    --argjson tty "$TTY" \
    --arg network "$([ "$NET" = none ] && echo none || echo inherit)" \
    --argjson extra "$(jq --arg n "$NAME" --arg b "$E_BACKEND" '.environments[$n].backendArgs[$b] // []' "$CONFIG")" \
    --argjson nm "${#M[@]}" \
    --argjson ne "${#envs[@]}" \
    --args '
      $ARGS.positional as $a
      | {
          version: 1,
          name: $name,
          hostname: $hostname,
          uid: $uid,
          gid: $gid,
          home: $home,
          cwd: $cwd,
          tty: $tty,
          network: $network,
          mounts: [$a[:$nm] | range(0; length; 4) as $i | .[$i:$i + 4]
            | {type: .[0], source: .[1], target: .[2], mode: .[3]}],
          env: ($a[$nm:$nm + $ne] | [range(0; length; 2) as $i | {key: .[$i], value: .[$i + 1]}] | from_entries),
          command: $a[$nm + $ne:],
          extraArgs: $extra
        }' -- "${M[@]}" "${envs[@]}" "$@"
}

# Write the network settings for the namespace process (refer to netns_setup).
write_net_settings() {
  local ips=() h ip
  for h in "${E_ALLOW_HOSTS[@]}"; do
    while read -r ip _; do ips+=("$ip"); done < <(getent ahosts "$h" | sort -u)
    [ ${#ips[@]} -gt 0 ] || warn "cannot resolve $h"
  done
  local proxy_host=""
  if [ -n "$E_SOCKS" ]; then
    # "HOST:PORT", as in sandboxed-agents, is a SOCKS5 proxy.
    [[ $E_SOCKS == *://* ]] || E_SOCKS=socks5://$E_SOCKS
    proxy_host=${E_SOCKS#*://}
    proxy_host=${proxy_host%:*}
    proxy_host=${proxy_host#[}
    proxy_host=${proxy_host%]}
  fi
  {
    declare -p ips
    printf 'internet=%q lan=%q socks=%q proxy_host=%q backend=%q\n' \
      "$E_INTERNET" "$E_LAN" "$E_SOCKS" "$proxy_host" "$BACKEND"
    printf 'name=%q opensnitch=%q osn_daemon=%q osn_config=%q osn_rules=%q osn_dir=%q\n' \
      "$NAME" "$E_OPENSNITCH" "$E_OSN_DAEMON" "$E_OSN_CONFIG" "$E_OSN_RULES" "$E_OSN_DIR"
  } >"$TMP/net.sh"
}

# Set up the new network namespace: the loopback interface, the SOCKS proxy
# and the firewall. The settings are the variables of net.sh. T2S is the PID
# of tun2socks.
netns_setup() {
  ip link set lo up

  # ping uses an ICMP socket without privileges (there is no setuid and no
  # cap_net_raw in a sandbox). A new network namespace allows that socket for
  # no group. Group 0 here is the group of the user on the host.
  # slirp4netns sends the echo requests on the host with the same socket type.
  echo "0 0" >/proc/sys/net/ipv4/ping_group_range 2>/dev/null || true

  T2S=""
  if [ -n "$socks" ]; then
    ip tuntap add mode tun dev tun0
    ip addr add 198.18.0.1/15 dev tun0
    ip link set dev tun0 up
    ip route add "$proxy_host" via 10.0.2.2 dev tap0 2>/dev/null || true
    ip route replace default dev tun0
    tun2socks -device tun0 -proxy "$socks" >/dev/null 2>&1 &
    T2S=$!
    # The firewall must not stop the connections to the proxy.
    ips+=("$proxy_host")
  fi

  local private4="10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 100.64.0.0/10 169.254.0.0/16"
  local private6="fc00::/7 fe80::/10"
  fw() { # fw IPTABLES ALLOW-NETS DENY-NETS
    local cmd=$1 n
    $cmd -A OUTPUT -o lo -j ACCEPT
    for n in "${ips[@]}"; do $cmd -A OUTPUT -d "$n" -j ACCEPT 2>/dev/null || true; done
    for n in $2; do $cmd -A OUTPUT -d "$n" -j ACCEPT; done
    for n in $3; do $cmd -A OUTPUT -d "$n" -j REJECT; done
  }
  if [ "$internet" = false ]; then
    # Only the local networks. 10.0.2.3 is the name server of slirp4netns.
    fw iptables "$private4" ""
    fw ip6tables "$private6" ""
    iptables -P OUTPUT DROP
    ip6tables -P OUTPUT DROP
  elif [ "$lan" = false ]; then
    # Only the internet. The name server of slirp4netns stays open.
    fw iptables "10.0.2.3/32" "$private4"
    fw ip6tables "" "$private6"
  fi
}

# Start the OpenSnitch daemon of the sandbox in this network namespace. The
# daemon sees each connection of the programs of the sandbox before
# slirp4netns, and it asks the GUI of the host. LOG is the log file; with no
# LOG, the daemon writes to the standard output. OSN_PID is the PID of the
# daemon.
#
# The function returns when the queue rules of the daemon are in place, so
# that no connection goes out before the daemon sees it.
opensnitch_start() {
  local log=${1:-} dir=$osn_dir
  mkdir -p "$dir/rules"
  chmod 0700 "$dir"

  # The rules of the configuration are links into the store. Remove the
  # links of old rules, and then add the links of the rules of the current
  # configuration. The rules of the GUI are files, and they stay.
  find "$dir/rules" -maxdepth 1 -type l -lname '/nix/store/*' -delete
  local f
  for f in "$osn_rules"/*.json; do
    [ -e "$f" ] && ln -sfn "$(readlink -f "$f")" "$dir/rules/${f##*/}"
  done

  # The configuration of the module, with the name of the node, the rules
  # and the log of this sandbox. The GUI shows the node as unix:/NAME.
  jq --arg name "$name" --arg rules "$dir/rules" --arg log "$log" '
      .Server.Name = $name
      | .Server.LogFile = $log
      | .Rules.Path = $rules' "$osn_config" >"$dir/config.json"

  # The daemon writes to the standard output until it reads the
  # configuration. For a run, the standard output is the terminal.
  if [ -n "$log" ]; then
    "$osn_daemon" -config-file "$dir/config.json" </dev/null >>"$log" 2>&1 &
  else
    "$osn_daemon" -config-file "$dir/config.json" </dev/null &
  fi
  OSN_PID=$!

  local n=0
  until nft list chain inet opensnitch mangle_output 2>/dev/null | grep -q queue; do
    if ! kill -0 "$OSN_PID" 2>/dev/null; then
      warn "opensnitchd stopped${log:+; refer to $log}"
      return 1
    fi
    n=$((n + 1))
    if [ $n -ge 2000 ]; then
      warn "opensnitchd did not set its queue rules in 20 s"
      kill "$OSN_PID" 2>/dev/null || true
      return 1
    fi
    sleep 0.01
  done
}

# The process in the network namespace of a run. It waits until slirp4netns
# makes tap0, sets up the namespace, and then starts the backend.
netns_inner() {
  local dir=$1
  read -r _ <"$dir/go"
  local ips=() internet lan socks proxy_host backend
  local name opensnitch osn_daemon osn_config osn_rules osn_dir
  # shellcheck source=/dev/null
  . "$dir/net.sh"

  netns_setup
  OSN_PID=""
  if [ "$opensnitch" = true ]; then
    opensnitch_start "$osn_dir/opensnitchd.log" || exit 1
  fi

  local rc=0
  "$backend" run "$dir/plan.json" || rc=$?
  [ -z "$OSN_PID" ] || kill "$OSN_PID" 2>/dev/null || true
  [ -z "$T2S" ] || kill "$T2S" 2>/dev/null || true
  exit "$rc"
}

# The process in the shared network namespace of a sandbox (the service
# sandbox-net@NAME). It sets up the namespace, starts the OpenSnitch daemon,
# and then waits. If the daemon stops, it starts the daemon again; if that
# fails, it stops, and the service fails.
netns_hold() {
  local dir=$1
  read -r _ <"$dir/go"
  local ips=() internet lan socks proxy_host backend
  local name opensnitch osn_daemon osn_config osn_rules osn_dir
  # shellcheck source=/dev/null
  . "$dir/net.sh"

  netns_setup
  # Stop the daemon and tun2socks with this process. (systemd stops all
  # processes of the service, but a manual start has no cgroup.)
  trap 'kill $OSN_PID $T2S 2>/dev/null; exit 0' TERM INT
  opensnitch_start || exit 1
  : >"$dir/up"
  while :; do
    wait "$OSN_PID" || true
    warn "opensnitchd stopped: start it again"
    sleep 1
    opensnitch_start || exit 1
  done
}

# Start "$SELF" WHAT "$TMP" in a new network namespace, and connect the
# namespace to the network of the host with slirp4netns. NS_PID is the PID of
# the process in the namespace, SLIRP_PID the PID of slirp4netns.
netns_start() {
  local what=$1
  write_net_settings
  mkfifo "$TMP/go" "$TMP/ready"

  # The namespace process gets the standard input of the caller (for a run,
  # the terminal).
  unshare --user --map-root-user --net -- "$SELF" "$what" "$TMP" <&0 &
  NS_PID=$!

  # Wait until the process is in the new namespace. Before that,
  # /proc/PID/ns/net is the namespace of the host.
  local self_ns n=0
  self_ns=$(readlink /proc/self/ns/net)
  while [ "$(readlink "/proc/$NS_PID/ns/net" 2>/dev/null || echo "$self_ns")" = "$self_ns" ]; do
    kill -0 "$NS_PID" 2>/dev/null || die "the network namespace did not start"
    n=$((n + 1))
    [ $n -lt 500 ] || die "the network namespace did not start"
    sleep 0.01
  done

  local loopback=(--disable-host-loopback)
  [ "$E_HOST_LOOPBACK" = true ] && loopback=()
  slirp4netns --configure --mtu=65520 "${loopback[@]}" --ready-fd=3 \
    "$NS_PID" tap0 3>"$TMP/ready" >/dev/null 2>&1 &
  SLIRP_PID=$!
  # slirp4netns writes "1" without a new line.
  read -r -t 5 -N 1 _ <"$TMP/ready" || warn "slirp4netns did not start: the sandbox has no network"
  echo go >"$TMP/go"
}

# Start the backend in a new network namespace with slirp4netns.
run_private() {
  netns_start __netns
  local rc=0
  wait "$NS_PID" || rc=$?
  kill "$SLIRP_PID" 2>/dev/null || true
  return "$rc"
}

# The runtime directory of the service sandbox-net@NAME.
net_dir() { printf '%s/sandbox-net-%s\n' "${XDG_RUNTIME_DIR:-/tmp}" "$1"; }

# The service sandbox-net@NAME: the network namespace of the sandbox NAME, for
# all runs of NAME. The process in the namespace writes $dir/up when the
# namespace and the OpenSnitch daemon are ready. Then this process writes the
# PID of that process to $dir/pid, and a run enters its namespace (refer to
# run_shared).
cmd_netd() {
  [ $# -eq 1 ] || die "usage: sandbox __netd NAME"
  load_env "$1"
  [ "$E_OPENSNITCH" = true ] || die "the sandbox $NAME has no opensnitch, so each run has its own network"
  TMP=$(net_dir "$NAME")
  rm -rf "$TMP"
  mkdir -m 0700 "$TMP"
  NS_PID="" SLIRP_PID=""
  trap 'kill $NS_PID $SLIRP_PID 2>/dev/null; rm -rf "$TMP"' EXIT
  trap 'exit 0' TERM INT

  # slirp4netns gets the label in its environment. A rule of the daemon of
  # the host can then find the slirp4netns of this sandbox (allowOnHost).
  export SANDBOX_NAME=$NAME SANDBOX_COLOR=$E_COLOR
  netns_start __netns_hold </dev/null

  local n=0
  until [ -e "$TMP/up" ]; do
    kill -0 "$NS_PID" 2>/dev/null || die "the network namespace of $NAME stopped"
    n=$((n + 1))
    [ $n -lt 3000 ] || die "the network namespace of $NAME was not ready in 30 s"
    sleep 0.01
  done
  echo "$NS_PID" >"$TMP/pid"
  echo "sandbox: the network of $NAME is ready"

  # Stop when the namespace process or slirp4netns stops.
  local rc=0
  wait -n "$NS_PID" "$SLIRP_PID" || rc=$?
  kill "$NS_PID" "$SLIRP_PID" 2>/dev/null || true
  die "the network of $NAME stopped (exit code $rc)"
}

# Start the backend in the network namespace of the service sandbox-net@NAME.
# The scope of the run requires the service, so systemd starts it first (refer
# to cmd_run).
run_shared() {
  local dir pid="" n=0
  dir=$(net_dir "$NAME")
  until [ -s "$dir/pid" ] && pid=$(<"$dir/pid") && kill -0 "$pid" 2>/dev/null; do
    local hint="refer to: journalctl --user -u sandbox-net@$NAME"
    systemctl --user -q is-active "sandbox-net@$NAME.service" ||
      die "the network of $NAME did not start; $hint"
    n=$((n + 1))
    [ $n -lt 3000 ] || die "the network of $NAME was not ready in 30 s; $hint"
    sleep 0.01
  done
  # The namespace has no setgroups, so keep the credentials: the user is
  # root in the namespace.
  nsenter --user --net --preserve-credentials --target "$pid" -- "$BACKEND" run "$TMP/plan.json"
}

cmd_run() {
  local need_cwd=()
  if [ "${1:-}" = --need-cwd ]; then
    need_cwd=(--need-cwd)
    shift
  fi
  [ $# -gt 0 ] || die "usage: sandbox run [--need-cwd] NAME [--] [CMD...]"
  local name=$1
  shift
  [ "${1:-}" = "--" ] && shift
  [ $# -gt 0 ] || set -- "${SHELL:-/bin/sh}"
  has_env "$name" || die "no sandbox has the name '$name'. Run 'sandbox list'."

  # Put each run in a scope below sandbox-NAME.slice. A scope keeps the
  # environment of the caller.
  if [ -z "${SANDBOX_NO_SCOPE:-}" ] && [ -z "${SANDBOX__SCOPED:-}" ] &&
    [ -S "${XDG_RUNTIME_DIR:-/nonexistent}/systemd/private" ]; then
    # With opensnitch, the run requires the network of the sandbox:
    # systemd starts sandbox-net@NAME before the scope, and stops it after
    # the last scope.
    local net=()
    if jq -e --arg n "$name" '.environments[$n].opensnitch.enable' "$CONFIG" >/dev/null; then
      net=(--property="Requires=sandbox-net@$name.service" --property="After=sandbox-net@$name.service")
    fi
    exec systemd-run --user --scope --quiet --collect "${net[@]}" \
      --slice="sandbox-$name.slice" --unit="sandbox-$name-$$-$RANDOM" \
      --description="sandbox $name: $*" \
      -- env SANDBOX__SCOPED=1 "$SELF" run "${need_cwd[@]}" "$name" -- "$@"
  fi

  load_env "$name"
  TMP=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/sandbox-$name.XXXXXX")
  printf 'nameserver 10.0.2.3\n' >"$TMP/resolv.conf"
  build_plan
  if [ ${#need_cwd[@]} -gt 0 ] && [ "$HAS_CWD" = false ]; then
    rm -rf "$TMP"
    die "the sandbox $NAME does not have the current directory $PWD"
  fi
  make_plan_json "${INIT[@]}" "$@" >"$TMP/plan.json"

  # The label is also in the environment of the backend and of slirp4netns.
  # The OpenSnitch rule of the host (allowOnHost) finds slirp4netns by
  # SANDBOX_NAME. A window manager reads the cgroup, not this environment.
  export SANDBOX_NAME=$NAME SANDBOX_COLOR=$E_COLOR

  # Ctrl-C goes to the program in the sandbox. This shell stays to remove the
  # files of the run after the program.
  trap : INT
  local rc=0
  if [ "$NET" = private ]; then
    run_private || rc=$?
  elif [ "$NET" = shared ]; then
    run_shared || rc=$?
  else
    "$BACKEND" run "$TMP/plan.json" || rc=$?
  fi
  if [ "$E_CLEAN_TMP" = true ]; then
    rm -rf "$TMP"
  else
    warn "the files of the run are in $TMP (cleanTmp is false)"
  fi
  exit "$rc"
}

cmd_show() {
  [ $# -eq 1 ] || die "usage: sandbox show NAME"
  load_env "$1"
  TMP="\$TMP"
  build_plan
  local plan
  plan=$(mktemp "${XDG_RUNTIME_DIR:-/tmp}/sandbox-show.XXXXXX")
  make_plan_json "${INIT[@]}" CMD >"$plan"
  echo "backend: $E_BACKEND ($BACKEND)"
  echo "network: $NET"
  jq . "$plan"
  "$BACKEND" show "$plan"
  rm -f "$plan"
}

swatch() { # swatch "#rrggbb": print a block in the color, for a terminal.
  local c=${1#\#}
  if [ -t 1 ] && [ ${#c} -eq 6 ]; then
    printf '\033[48;2;%d;%d;%dm  \033[0m ' "0x${c:0:2}" "0x${c:2:2}" "0x${c:4:2}"
  fi
}

cmd_list() {
  case ${1:-} in
    --names)
      names
      return
      ;;
    --json)
      jq .environments "$CONFIG"
      return
      ;;
  esac
  local name color backend net caps desc
  while IFS=$'\t' read -r name color backend net caps desc; do
    swatch "$color"
    printf '%-12s %-9s %-7s %-10s %-36s %s\n' "$name" "$color" "$backend" "$net" "$caps" "$desc"
  done < <(jq -r '.environments | to_entries[] | .value as $e | [
      .key,
      $e.color,
      $e.backend,
      (if $e.disableNetworking then "none"
       elif $e.shareHostNetwork then "host"
       elif $e.socksProxy != null then "socks"
       elif ($e.internetAccess | not) then "lan-only"
       elif ($e.lanAccess | not) then "inet-only"
       else "internet" end),
      ([ "gui", "x11", "audio", "ssh", "gpg", "git", "claude", "codex", "gemini", "hermes",
         "opencode", "zed", "direnv", "docker", "dbus", "nix", "fuse", "kvm", "nvidia", "libvirt",
         "allowHome", "privacyFilter", "yolo" ]
        | map(. as $k | $e[$k] | if . == true or . == "private" or . == "read" then $k
            elif . == "host" then "\($k):host" elif . == "write" then "\($k):rw"
            else empty end) | join(",")),
      $e.description
    ] | @tsv' "$CONFIG")
}

cmd_pick() {
  local host=false gui=false a
  for a in "$@"; do
    case $a in
      --host) host=true ;;
      --gui) gui=true ;;
      *) die "usage: sandbox pick [--host] [--gui]" ;;
    esac
  done
  local menu markup
  menu=$(jq -r .menu.command "$CONFIG")
  markup=$(jq -r .menu.markup "$CONFIG")
  local choice
  choice=$(
    {
      if [ "$host" = true ]; then
        if [ "$markup" = true ]; then
          jq -r '"<span background=\"\(.host.color)\" foreground=\"#ffffff\"> host </span>  no sandbox"' "$CONFIG"
        else
          printf 'host  no sandbox\n'
        fi
      fi
      if [ "$markup" = true ]; then
        jq -r --argjson gui "$gui" '.environments | to_entries[] | select(($gui | not) or .value.gui)
          | "<span background=\"\(.value.color)\" foreground=\"#ffffff\"> \(.key) </span>  "
            + (.value.description | gsub("&"; "&amp;") | gsub("<"; "&lt;") | gsub(">"; "&gt;"))' "$CONFIG"
      else
        jq -r --argjson gui "$gui" '.environments | to_entries[] | select(($gui | not) or .value.gui)
          | "\(.key)  \(.value.description)"' "$CONFIG"
      fi
    } | sh -c "$menu"
  ) || exit 1
  choice=$(printf '%s' "$choice" | sed -e 's/<[^>]*>//g' | awk '{ print $1 }')
  [ -n "$choice" ] || exit 1
  if [ "$choice" != host ]; then
    has_env "$choice" || die "no sandbox has the name '$choice'"
  fi
  printf '%s\n' "$choice"
}

cmd_ps() {
  local unit pid desc
  printf '%-12s %-8s %s\n' SANDBOX PID COMMAND
  while read -r unit _; do
    [ -n "$unit" ] || continue
    local name=${unit#sandbox-}
    name=${name%%-*}
    pid=$(systemctl --user show -p MainPID --value "$unit" 2>/dev/null || echo "?")
    desc=$(systemctl --user show -p Description --value "$unit" 2>/dev/null || echo "")
    printf '%-12s %-8s %s\n' "$name" "$pid" "${desc#sandbox "$name": }"
  done < <(systemctl --user list-units --type=scope --state=running --no-legend --plain 'sandbox-*')
}

# Run a program on the host with the label of the host. A shell prompt reads
# the label as for a sandbox. A window manager finds the host from the
# cgroup. The scope below app.slice takes the program out of the cgroup of
# the caller (for example the window manager). The name follows the XDG
# convention app-<launcher>-<id>-<random>.scope.
cmd_host() {
  [ "${1:-}" = "--" ] && shift
  [ $# -gt 0 ] || set -- "${SHELL:-/bin/sh}"
  if [ -z "${SANDBOX_NO_SCOPE:-}" ] && [ -z "${SANDBOX__SCOPED:-}" ] &&
    [ -S "${XDG_RUNTIME_DIR:-/nonexistent}/systemd/private" ]; then
    exec systemd-run --user --scope --quiet --collect --slice=app.slice \
      --unit="app-sandbox-host-$$-$RANDOM" --description="host: $*" \
      -- env SANDBOX__SCOPED=1 "$SELF" host -- "$@"
  fi
  SANDBOX_COLOR=$(jq -r .host.color "$CONFIG")
  export SANDBOX_NAME=host SANDBOX_COLOR
  # A program on the host can start runs, and each run must start its scope.
  unset SANDBOX_BACKEND SANDBOX__SCOPED
  exec "$@"
}

# The sandbox of this process. A program on the host can set SANDBOX_NAME,
# thus PID 1 must not be systemd: in a sandbox, PID 1 is bwrap (bwrap) or the
# first program of the sandbox (gVisor).
cmd_current() {
  local name=${SANDBOX_NAME:-host}
  if [ "$name" != host ] && [ "$(cat /proc/1/comm 2>/dev/null)" = systemd ]; then
    name=host
  fi
  printf '%s\n' "$name"
}

cmd_stop() {
  [ $# -eq 1 ] || die "usage: sandbox stop NAME"
  systemctl --user stop "sandbox-$1.slice"
}

main() {
  local cmd=${1:-}
  [ $# -gt 0 ] && shift
  case $cmd in
    __netns)
      # netns_inner does not return.
      netns_inner "$@"
      ;;
    __netns_hold)
      # netns_hold does not return.
      netns_hold "$@"
      ;;
    current)
      cmd_current
      return
      ;;
    "" | -h | --help | help)
      usage
      return
      ;;
  esac
  require_config
  case $cmd in
    list) cmd_list "$@" ;;
    show) cmd_show "$@" ;;
    run) cmd_run "$@" ;;
    pick) cmd_pick "$@" ;;
    ps) cmd_ps ;;
    stop) cmd_stop "$@" ;;
    host) cmd_host "$@" ;;
    __netd) cmd_netd "$@" ;;
    backends) jq -r '.backends | to_entries[] | "\(.key)\t\(.value)"' "$CONFIG" ;;
    home)
      load_env "${1:?usage: sandbox home NAME}"
      printf '%s\n' "$E_HOME_DIR"
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
}

main "$@"
