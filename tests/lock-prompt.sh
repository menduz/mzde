# Tests the place of the waylock password prompt after a change of the scale
# or the mode of the output: river with the headless backend of wlroots, kwm,
# and waylock. wlr-randr changes the output, as kanshi does after a resume.
#
# river must send a new configure event to the lock surface, and waylock must
# place the prompt in the center of the new size. Run it with the environment
# of lock-prompt.nix.
set -euo pipefail

work=$(mktemp -d)
cleanup() {
  # shellcheck disable=SC2046 # one argument for each job
  kill $(jobs -p) 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

unset DISPLAY WAYLAND_DISPLAY
export HOME=$work/home
export XDG_CONFIG_HOME=$work/config
export XDG_RUNTIME_DIR=$work/run
mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"

export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_HEADLESS_OUTPUTS=1
export WLR_LIBINPUT_NO_DEVICES=1

river -no-xwayland -c kwm >"$work/river.log" 2>&1 &
for _ in $(seq 100); do
  socket=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit)
  [ -n "$socket" ] && break
  sleep 0.1
done
if [ -z "${socket:-}" ]; then
  echo "lock-prompt: river did not start" >&2
  cat "$work/river.log" >&2
  exit 1
fi
WAYLAND_DISPLAY=$(basename "$socket")
export WAYLAND_DISPLAY
sleep 1

waylock -init-color 0x000000 >"$work/waylock.log" 2>&1 &

failed=0

# expect_center <description>: the prompt is in the center of the output, at
# most 4 pixels away (the glyphs are not symmetric).
expect_center() {
  local offset=none
  for _ in $(seq 25); do
    sleep 0.2
    grim "$work/shot.png"
    offset=$(python3 "$PROMPT_CENTER" "$work/shot.png")
    if [ "$offset" != none ]; then
      read -r dx dy <<<"$offset"
      if [ "${dx#-}" -le 4 ] && [ "${dy#-}" -le 4 ]; then
        echo "ok: $1"
        return
      fi
    fi
  done
  echo "FAIL: $1 (offset of the prompt: $offset)"
  failed=1
}

expect_center "the prompt is in the center of the output"
wlr-randr --output HEADLESS-1 --scale 2
expect_center "after a change of the scale, the prompt is in the center"
wlr-randr --output HEADLESS-1 --custom-mode 1920x1080
expect_center "after a change of the mode, the prompt is in the center"
wlr-randr --output HEADLESS-1 --scale 1
expect_center "after a change back to scale 1, the prompt is in the center"
wlr-randr --output HEADLESS-1 --custom-mode 1280x720 --scale 2
expect_center "after a change of the mode and the scale together, the prompt is in the center"

if [ "$failed" -ne 0 ]; then
  echo "lock-prompt: the log of waylock:" >&2
  cat "$work/waylock.log" >&2
  exit 1
fi
echo "lock-prompt: all tests passed"
