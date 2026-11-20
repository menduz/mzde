# sandbox-init: the first program in a sandbox. It starts the private services
# that the plan asks for, and then it replaces itself with the command.
#
#   SANDBOX_INIT_DBUS=1          a private session bus
#   SANDBOX_INIT_X11=1           a private X server (xwayland-satellite)
#   SANDBOX_INIT_X11_DISPLAY=N   the first display number to try
#   SANDBOX_INIT_HOLD=1          the first process of the container of a
#                                sandbox: start the services, write their
#                                variables to $XDG_RUNTIME_DIR/sandbox-init.env,
#                                and stay until the container stops
#   SANDBOX_INIT_JOIN=1          a run in the container: read the variables of
#                                the services from that file, and then start
#                                the command
#
# The services stop with the sandbox: the backend stops all processes of the
# sandbox when the command (or, in a container, this process) stops. The
# services do not get the terminal: a terminal stays open while a process has
# it, and they must not read from it.

set -euo pipefail

envfile=${XDG_RUNTIME_DIR:-/tmp}/sandbox-init.env

if [ -n "${SANDBOX_INIT_JOIN:-}" ]; then
  unset SANDBOX_INIT_JOIN
  # The backend starts a run when the container has started this program,
  # but the services can need more time.
  for _ in $(seq 200); do
    [ -e "$envfile" ] && break
    sleep 0.05
  done
  if [ -e "$envfile" ]; then
    # shellcheck source=/dev/null
    . "$envfile"
  else
    echo "sandbox-init: the services of the sandbox are not ready; the program starts without them" >&2
  fi
  exec "$@"
fi

if [ -n "${SANDBOX_INIT_DBUS:-}" ]; then
  addr="unix:path=$XDG_RUNTIME_DIR/bus"
  # --fork returns when the bus is ready.
  dbus-daemon --config-file="$DBUS_SESSION_CONF" --address="$addr" \
    --fork --nopidfile --print-address=1 </dev/null >/dev/null 2>"$XDG_RUNTIME_DIR/dbus-daemon.log"
  export DBUS_SESSION_BUS_ADDRESS=$addr
fi

if [ -n "${SANDBOX_INIT_X11:-}" ] && [ -n "${WAYLAND_DISPLAY:-}" ]; then
  n=${SANDBOX_INIT_X11_DISPLAY:-0}
  while [ -e "/tmp/.X11-unix/X$n" ] || [ -e "/tmp/.X$n-lock" ]; do n=$((n + 1)); done
  xwayland-satellite ":$n" </dev/null >"$XDG_RUNTIME_DIR/xwayland-satellite.log" 2>&1 &
  for _ in $(seq 100); do
    [ -S "/tmp/.X11-unix/X$n" ] && break
    sleep 0.05
  done
  if [ -S "/tmp/.X11-unix/X$n" ]; then
    export DISPLAY=":$n"
  else
    echo "sandbox-init: xwayland-satellite did not start; refer to $XDG_RUNTIME_DIR/xwayland-satellite.log" >&2
  fi
fi

unset SANDBOX_INIT_DBUS SANDBOX_INIT_X11 SANDBOX_INIT_X11_DISPLAY

if [ -n "${SANDBOX_INIT_HOLD:-}" ]; then
  # Write the file in one step: a run must not read half of it.
  {
    [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] || printf 'export DBUS_SESSION_BUS_ADDRESS=%q\n' "$DBUS_SESSION_BUS_ADDRESS"
    [ -z "${DISPLAY:-}" ] || printf 'export DISPLAY=%q\n' "$DISPLAY"
  } >"$envfile.tmp"
  mv "$envfile.tmp" "$envfile"
  # In gVisor this is process 1, and it gets the processes of the runs that
  # lose their parent. bash removes them (wait), so they do not stay as
  # zombies.
  trap 'exit 0' TERM INT
  while :; do
    sleep 3600 &
    wait $! || true
  done
fi

exec "$@"
