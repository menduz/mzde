# sandbox-backend-gvisor: start a plan of `sandbox` with gVisor (runsc).
#
# Usage: sandbox-backend-gvisor run|start|exec|show PLAN.json
#
# gVisor gives the program a kernel in user space (the Sentry). Only a small
# set of system calls goes to the kernel of the host. Refer to README.md for
# the format of the plan.
#
# Limits:
#   - gVisor has no /dev/dri, /dev/snd, /dev/kvm or /dev/fuse of the host. The
#     backend ignores device mounts, so a GUI program draws without a GPU.
#   - NVIDIA devices work through nvproxy (SANDBOX_RUNSC_NVPROXY=1).
#   - gVisor has its own /sys.
#   - The program runs as root in gVisor (refer to the OCI configuration).
#   - No Wayland or X11 client can draw: gVisor cannot give its memory files
#     (the window buffers) to the compositor of the host.

set -euo pipefail

usage() {
  echo "usage: sandbox-backend-gvisor run|start|exec|show PLAN.json" >&2
  exit 2
}
[ $# -eq 2 ] || usage
mode=$1 plan=$2

name=$(jq -r .name "$plan")
network=$(jq -r .network "$plan")

# The bundle (config.json, rootfs) and the state of runsc (state). For "run"
# and "show", a directory for this run; for "start" and "exec", the state
# directory of the container. "exec" uses a directory of its own for its
# process.json.
case $mode in
  start | exec) dir=$(jq -r .state "$plan") ;;
  run | show)
    dir=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/sandbox-runsc-$name.XXXXXX")
    trap 'rm -rf "$dir"' EXIT
    ;;
  *) usage ;;
esac
mkdir -p "$dir/rootfs" "$dir/state"
work=$dir
if [ "$mode" = exec ]; then
  work=$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/sandbox-runsc-exec-$name.XXXXXX")
  trap 'rm -rf "$work"' EXIT
elif [ "$mode" != show ]; then
  for dev in $(jq -r '.mounts[] | select(.type == "dev") | .source' "$plan"); do
    echo "sandbox-backend-gvisor: gVisor cannot use $dev; the sandbox does not get it" >&2
  done
fi

# With a terminal, runsc gives the terminal of the host to the program. The
# terminal of the host then sends the signals of Ctrl-C and of a resize to
# runsc, not to the program, and Ctrl-C stops the whole sandbox. Thus
# `script` makes a terminal in gVisor and copies the bytes between the two
# terminals. The terminal of gVisor sends the signals to the program. The
# backend sends SIGWINCH to all processes of the sandbox when the terminal of
# the host changes its size (refer to "run" below). A process without a
# handler ignores SIGWINCH.
tty=$(jq -r .tty "$plan")
if [ "$tty" = true ]; then
  script=$(command -v script)
  # `script -- CMD ARGS` joins the words without quotes, and $SHELL runs the
  # result. Give it one command with the quotes of a shell. As process 1 of
  # gVisor, `script` does not stop after the command, so a shell is process 1.
  # The "exit" prevents that the shell replaces itself with `script`.
  jq --arg script "$script" '
    .command = ["/bin/sh", "-c", "\"$@\"; exit $?", "sh",
                $script, "-qef", "-c", (.command | @sh), "/dev/null"]' \
    "$plan" >"$work/plan.json"
  plan=$work/plan.json
fi

# The OCI runtime configuration. The program runs as root (uid 0) in gVisor.
# runsc --rootless maps that root to the user that starts runsc, so the files
# of the program belong to the user on the host. A user ID other than 0 in
# gVisor sees the files of the host as files of root.
# The process of the plan, for config.json and for `runsc exec`.
process='
  def process: {
    terminal: .tty,
    user: {uid: 0, gid: 0},
    args: .command,
    env: (.env | to_entries | map("\(.key)=\(.value)")),
    cwd: .cwd,
    capabilities: {bounding: [], effective: [], permitted: [], inheritable: [], ambient: []},
    rlimits: [{type: "RLIMIT_NOFILE", hard: 65536, soft: 65536}],
    noNewPrivileges: true
  };'
if [ "$mode" = exec ]; then
  jq "$process"' process' "$plan" >"$work/process.json"
else
  jq "$process"'
  . as $p
  | {
      ociVersion: "1.0.2",
      process: process,
      root: {path: "rootfs", readonly: true},
      hostname: $p.hostname,
      mounts: (
        [
          {destination: "/proc", type: "proc", source: "proc"},
          {destination: "/dev", type: "tmpfs", source: "tmpfs", options: ["nosuid", "strictatime", "mode=755"]},
          {destination: "/dev/shm", type: "tmpfs", source: "shm", options: ["nosuid", "nodev", "mode=1777"]},
          {destination: "/dev/pts", type: "devpts", source: "devpts",
           options: ["nosuid", "noexec", "newinstance", "ptmxmode=0666", "mode=0620"]},
          {destination: "/sys", type: "sysfs", source: "sysfs", options: ["nosuid", "noexec", "nodev", "ro"]}
        ]
        + [$p.mounts[]
            | select(.type != "dev" and .target != "/sys")
            | if .type == "bind" then
                {destination: .target, type: "bind", source: .source,
                 options: (["rbind", "rprivate"] + (if .mode == "ro" then ["ro"] else [] end))}
              elif .type == "tmpfs" then
                {destination: .target, type: "tmpfs", source: "tmpfs",
                 options: ["nosuid", "nodev", "mode=\(.mode)"]}
              else error("unknown mount type \(.type)") end]
      ),
      linux: {
        namespaces: [{type: "pid"}, {type: "ipc"}, {type: "uts"}, {type: "mount"}]
      }
    }' "$plan" >"$dir/config.json"
fi

mapfile -t extra < <(jq -r '.extraArgs[]' "$plan")

flags=(
  --root="$dir/state"
  --rootless
  --ignore-cgroups
  --platform="${SANDBOX_RUNSC_PLATFORM:-systrap}"
  # The root file system is an empty directory. The overlay in memory takes
  # the mount points.
  --overlay2=root:memory
  # The program can connect to the sockets of the host that the plan mounts
  # (Wayland, SSH agent, Docker).
  --host-uds=open
)
# "inherit" uses the network namespace of the caller. `sandbox` makes that
# namespace with slirp4netns and the firewall.
if [ "$network" = none ]; then
  flags+=(--network=none)
else
  flags+=(--network=host)
fi
[ -z "${SANDBOX_RUNSC_NVPROXY:-}" ] || flags+=(--nvproxy)
flags+=("${extra[@]}")

# A run has a container of its own. The container of a sandbox has one name,
# and the state directory of the sandbox.
id="sandbox-$name-$$"
[ "$mode" = run ] || [ "$mode" = show ] || id="sandbox-$name"

# Stop the processes of this backend: all processes of the sandbox, or for
# "exec" the process of the run (its PID in gVisor).
stop() {
  local target=(--all "$id")
  if [ "$mode" = exec ]; then
    [ -s "$work/pid" ] || return 0
    target=(--pid "$(<"$work/pid")" "$id")
  fi
  runsc "${flags[@]}" kill "${target[@]}" SIGKILL 2>/dev/null || true
}

# Wait for the runsc process $pid. With a terminal, WINCH goes to all
# processes of the sandbox. A process without a handler ignores SIGWINCH.
wait_runsc() {
  # runsc does not stop the sandbox when it gets SIGTERM or SIGHUP. A sandbox
  # that stays uses CPU and memory, and no window or scope shows it.
  trap stop TERM HUP
  if [ "$tty" = true ]; then
    trap 'runsc "${flags[@]}" kill --all "$id" SIGWINCH 2>/dev/null || true' WINCH
  fi
  # A signal stops `wait` early. Wait again until runsc stops.
  while :; do
    rc=0
    wait "$pid" || rc=$?
    kill -0 "$pid" 2>/dev/null || break
  done
  exit "$rc"
}

case $mode in
  run)
    # runsc stays in the process group of the terminal: this shell has no job
    # control. The explicit redirection keeps the standard input (for example
    # the terminal).
    runsc "${flags[@]}" run --bundle "$dir" "$id" <&0 &
    pid=$!
    wait_runsc
    ;;
  start)
    rm -f "$dir/ready"
    runsc "${flags[@]}" run --bundle "$dir" "$id" </dev/null &
    pid=$!
    trap stop TERM HUP
    # `exec` can start programs when the first process runs.
    n=0
    until runsc "${flags[@]}" state "$id" 2>/dev/null | jq -e '.status == "running"' >/dev/null; do
      if ! kill -0 "$pid" 2>/dev/null || [ $((n += 1)) -ge 600 ]; then
        echo "sandbox-backend-gvisor: the container did not start" >&2
        stop
        exit 1
      fi
      sleep 0.05
    done
    : >"$dir/ready"
    wait_runsc
    ;;
  exec)
    runsc "${flags[@]}" exec --process "$work/process.json" \
      --internal-pid-file "$work/pid" "$id" <&0 &
    pid=$!
    wait_runsc
    ;;
  show)
    printf 'runsc'
    printf ' %q' "${flags[@]}" run --bundle "$dir" "$id"
    printf '\n'
    jq . "$dir/config.json"
    ;;
esac
