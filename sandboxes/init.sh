# sandbox-init: the first program in a sandbox. It starts the private services
# that the plan asks for, and then it replaces itself with the command.
#
#   SANDBOX_INIT_DBUS=1          a session bus for this run only
#   SANDBOX_INIT_X11=1           an X server for this run only (xwayland-satellite)
#   SANDBOX_INIT_X11_DISPLAY=N   the first display number to try
#
# The services stop with the sandbox: the backend stops all processes of the
# sandbox when the command stops. The services do not get the terminal: a
# terminal stays open while a process has it, and they must not read from it.

set -euo pipefail

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
exec "$@"
