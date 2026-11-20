# sandbox-backend-bwrap: start a plan of `sandbox` with bubblewrap.
#
# Usage: sandbox-backend-bwrap run|show PLAN.json
#
# bubblewrap gives Linux namespaces: the program uses the kernel of the host.
# Refer to README.md for the format of the plan.

set -euo pipefail

[ $# -eq 2 ] || {
  echo "usage: sandbox-backend-bwrap run|show PLAN.json" >&2
  exit 2
}
plan=$2

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
  show)
    printf 'bwrap'
    printf ' %q' "${args[@]}"
    printf '\n'
    ;;
  *)
    echo "usage: sandbox-backend-bwrap run|show PLAN.json" >&2
    exit 2
    ;;
esac
