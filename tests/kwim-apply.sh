# Tests that kwim applies its settings when river is slow, as after a resume.
# Run it with the environment of kwim-apply.nix.
#
# river sends input devices to kwim only for real devices, not for virtual
# ones. Thus the test runs a river in a river: the Wayland backend of wlroots
# makes a real keyboard from the keyboard of the outer river. The outer river
# has a virtual keyboard (wtype), and kwm gives the focus to the output window
# of the inner river.
#
# kwim sets the key repeat of the inner keyboard. A client of the inner river
# reads the key repeat (repeat_info.py). river sends it only after a key press
# on the new keyboard group, thus the test presses Shift first.
set -euo pipefail

work=$(mktemp -d)
cleanup() {
  # shellcheck disable=SC2046 # one argument for each job
  kill -CONT $(jobs -p) 2>/dev/null || true
  # shellcheck disable=SC2046
  kill $(jobs -p) 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

unset DISPLAY WAYLAND_DISPLAY
export HOME=$work/home
export XDG_CONFIG_HOME=$work/config
export XDG_RUNTIME_DIR=$work/run
mkdir -p "$HOME" "$XDG_CONFIG_HOME/kwim" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

export WLR_RENDERER=pixman
export WLR_LIBINPUT_NO_DEVICES=1

# wait_socket <name> <log>: waits until river makes the socket <name>.
wait_socket() {
  for _ in $(seq 100); do
    [ -S "$XDG_RUNTIME_DIR/$1" ] && return
    sleep 0.1
  done
  echo "kwim-apply: river did not make $1" >&2
  cat "$2" >&2
  exit 1
}

WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 \
  river -no-xwayland -c kwm >"$work/outer.log" 2>&1 &
wait_socket wayland-1 "$work/outer.log"
sleep 1

# The virtual keyboard of the outer river. It stays until the end.
WAYLAND_DISPLAY=wayland-1 wtype -s 3600000 x &
sleep 0.5

WAYLAND_DISPLAY=wayland-1 WLR_BACKENDS=wayland WLR_WL_OUTPUTS=1 \
  river -no-xwayland -c true >"$work/inner.log" 2>&1 &
inner=$!
wait_socket wayland-2 "$work/inner.log"
sleep 1
export WAYLAND_DISPLAY=wayland-2

# repeat: prints the key repeat of the inner river, "<rate> <delay>".
repeat() {
  WAYLAND_DISPLAY=wayland-1 wtype -k Shift_L
  sleep 0.3
  python3 "$REPEAT_INFO"
}

# configure <rate> <delay>: the kwim configuration with this key repeat.
configure() {
  printf '.{ .input_device_rules = .{ .{ .repeat_info = .{ .rate = %s, .delay = %s } } } }\n' \
    "$1" "$2" >"$XDG_CONFIG_HOME/kwim/config.zon"
}

failed=0

# expect_repeat <description> <rate> <delay>
expect_repeat() {
  local got
  got=$(repeat)
  if [ "$got" = "$2 $3" ]; then
    echo "ok: $1"
  else
    echo "FAIL: $1 (key repeat: $got, expected: $2 $3)"
    failed=1
  fi
}

expect_repeat "before kwim, the key repeat of river" 40 400

configure 77 222
timeout 10 kwim 2>"$work/kwim-1.log"
expect_repeat "kwim sets the key repeat" 77 222

# stall <rate> <delay> <event>: runs kwim with this key repeat, and stops river
# for 1 second when kwim gets the first <event> (a regular expression on the
# WAYLAND_DEBUG log). kwim must wait for river. If kwim disconnects first,
# river drops the requests that it did not read.
stall() {
  local log=$work/kwim-$1.log pid
  configure "$1" "$2"
  WAYLAND_DEBUG=1 timeout 10 kwim 2>"$log" &
  pid=$!
  for _ in $(seq 500); do
    grep -v -- '->' "$log" | grep -q "$3" && break
    sleep 0.01
  done
  kill -STOP "$inner"
  sleep 1
  kill -CONT "$inner"
  if ! wait "$pid"; then
    echo "FAIL: kwim stopped with an error"
    cat "$log"
    failed=1
  fi
}

# river stops after it sends the devices: kwim must wait before it
# disconnects. This script cannot stop river between the registry and the
# binds: that window is shorter than one poll of the log.
stall 55 333 'river_input_device_v1#[0-9]*\.name('
expect_repeat "kwim sets the key repeat when river stops after the devices" 55 333

if [ "$failed" -ne 0 ]; then
  echo "kwim-apply: the river log follows" >&2
  cat "$work/inner.log" >&2
  exit 1
fi
