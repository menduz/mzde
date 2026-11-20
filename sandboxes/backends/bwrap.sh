# sandbox-backend-bwrap: start a plan of `sandbox` with bubblewrap.
#
# Usage: sandbox-backend-bwrap run|start|exec|show PLAN.json
#
# bubblewrap gives Linux namespaces: the program uses the kernel of the host.
# Refer to README.md for the format of the plan.

set -euo pipefail

usage() {
  echo "usage: sandbox-backend-bwrap run|start|exec|show PLAN.json" >&2
  exit 2
}
[ $# -eq 2 ] || usage
plan=$2

# The start time of the process $1 (field 22 of /proc/PID/stat), so that a
# different process with the same PID is not the container. The name of the
# program (field 2) can have spaces, thus remove it first.
starttime() {
  local stat f
  stat=$(<"/proc/$1/stat") || return 1
  read -ra f <<<"${stat##*) }"
  printf '%s\n' "${f[19]}"
}

# exec: start the command of the plan in the container that `start` made for
# the same state directory. The program gets the environment and the
# directory of the plan, and the namespaces and the root of the container.
#
# With --dev, bwrap makes a second user namespace in the first one, and the
# first one has the other namespaces. Thus three nsenter: the first enters
# the parent of the user namespace of the container (the owner of the other
# namespaces), where the user is root; the second enters the other
# namespaces and the root of the container; the third enters the user
# namespace of the container, where the user is the user of the plan,
# without capabilities. One nsenter cannot do the first two: it enters the
# other namespaces before the parent user namespace.
if [ "$1" = exec ]; then
  state=$(jq -r .state "$plan")
  read -r pid start <"$state/pid" || {
    echo "sandbox-backend-bwrap: the container of $state is not ready" >&2
    exit 1
  }
  if [ "$(starttime "$pid" 2>/dev/null)" != "$start" ]; then
    echo "sandbox-backend-bwrap: the container of $state stopped" >&2
    exit 1
  fi
  ns=(--mount --pid --ipc --uts --cgroup)
  # With "inherit", the caller is in the network namespace of the container.
  [ "$(jq -r .network "$plan")" = none ] && ns+=(--net)
  # A program without a terminal cannot use TIOCSTI on the terminal of the
  # caller (as --new-session of bwrap).
  session=()
  [ "$(jq -r .tty "$plan")" = true ] || session=("$(command -v setsid)" --wait)
  mapfile -d '' -t env < <(jq -j '.env | to_entries[] | "\(.key)=\(.value)\u0000"' "$plan")
  mapfile -d '' -t cmd < <(jq -j '.command[] | . + "\u0000"' "$plan")
  # The program stops if this process stops (as --die-with-parent of bwrap).
  # The argument of --wdns is optional for nsenter: it must follow "=".
  # Process 1 of the container is the target in the third nsenter: the
  # /proc of the container does not show the PID of the host.
  nsenter=$(command -v nsenter)
  exec env -i "${env[@]}" "$nsenter" --target "$pid" --user-parent --preserve-credentials -- \
    "$nsenter" --target "$pid" "${ns[@]}" \
    --root --wdns="$(jq -r .cwd "$plan")" --preserve-credentials -- \
    "$nsenter" --target 1 --user --preserve-credentials -- \
    "$(command -v setpriv)" --no-new-privs --pdeathsig KILL -- \
    "${session[@]}" "${cmd[@]}"
fi

# Make the bwrap arguments from the plan, separated by NUL.
mapfile -d '' -t args < <(jq -j '
  [
    "--die-with-parent",
    "--unshare-user", "--unshare-ipc", "--unshare-pid", "--unshare-uts", "--unshare-cgroup-try",
    "--uid", (.uid | tostring), "--gid", (.gid | tostring),
    "--hostname", .hostname,
    # A program without a terminal cannot use TIOCSTI on the terminal of the
    # caller. A shell on a terminal must keep the terminal for job control.
    (if .tty then empty else "--new-session" end),
    (if .network == "none" then "--unshare-net" else empty end),
    "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/dev/shm",
    (.mounts[]
      | if .type == "bind" then (if .mode == "ro" then "--ro-bind" else "--bind" end), .source, .target
        elif .type == "dev" then "--dev-bind", .source, .target
        elif .type == "tmpfs" then "--perms", .mode, "--tmpfs", .target
        else error("unknown mount type \(.type)") end),
    "--clearenv",
    (.env | to_entries[] | "--setenv", .key, .value),
    "--chdir", .cwd,
    .extraArgs[],
    "--",
    .command[]
  ] | .[] | . + "\u0000"' "$plan")

case $1 in
  run) exec bwrap "${args[@]}" ;;
  start)
    state=$(jq -r .state "$plan")
    mkdir -p "$state"
    bwrap --info-fd 3 "${args[@]}" 3>"$state/info.json" &
    bwrap=$!
    trap 'kill "$bwrap" 2>/dev/null || true' TERM INT HUP
    # bwrap writes the PID of process 1 of the sandbox before process 1 makes
    # the root of the sandbox: before that, its root is the root of the host.
    # Process 1 starts the command after it made the root and all namespaces,
    # thus the container is ready when process 1 has a child.
    pid="" n=0
    until [ -n "$pid" ] && [ -n "$(pgrep -P "$pid")" ]; do
      if ! kill -0 "$bwrap" 2>/dev/null; then
        rc=0
        wait "$bwrap" || rc=$?
        echo "sandbox-backend-bwrap: bwrap stopped (exit code $rc)" >&2
        exit 1
      fi
      n=$((n + 1))
      [ $n -lt 3000 ] || {
        echo "sandbox-backend-bwrap: the container did not start in 30 s" >&2
        kill "$bwrap"
        exit 1
      }
      [ -n "$pid" ] || pid=$(jq -er '."child-pid"' "$state/info.json" 2>/dev/null) || pid=""
      sleep 0.01
    done
    printf '%s %s\n' "$pid" "$(starttime "$pid")" >"$state/pid.tmp"
    mv "$state/pid.tmp" "$state/pid"
    : >"$state/ready"
    # A signal stops `wait` early. Wait again until bwrap stops.
    while :; do
      rc=0
      wait "$bwrap" || rc=$?
      kill -0 "$bwrap" 2>/dev/null || break
    done
    exit "$rc"
    ;;
  show)
    printf 'bwrap'
    printf ' %q' "${args[@]}"
    printf '\n'
    ;;
  *) usage ;;
esac
