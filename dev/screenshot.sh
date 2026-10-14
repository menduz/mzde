# Takes a screenshot of mzde: river with the headless backend of wlroots, kwm
# with the configuration of the demo system, three tray items, Thunar and
# Ghostty. Run it with the environment of `mzde-screenshot` in screenshots.nix.
#
# Usage: mzde-screenshot <output directory> <variant> <settings file>
#
# The settings file gives HOME_FILES, GSETTINGS_KEYFILE, HOME_PATH, THEME_NAME
# and GHOSTTY_THEME of the variant. The script writes <output directory>/<variant>.png.
#
# The bar shows fixed values: the clock is 12:00, the CPU meter 25%, the
# memory meter 40%, and there is no battery. Thus the image changes only when
# the desktop changes.
set -euo pipefail

out=${1:?usage: mzde-screenshot <output directory> <variant> <settings file>}
variant=${2:?usage: mzde-screenshot <output directory> <variant> <settings file>}
settings=${3:?usage: mzde-screenshot <output directory> <variant> <settings file>}
# shellcheck source=/dev/null
. "$settings"
mkdir -p "$out"

# The places are always the same, so Thunar always shows the same path.
work=${TMPDIR:-/tmp}/mzde
rm -rf "$work"
mkdir -p "$work"

pids=()
cleanup() {
  local pid
  for pid in "${pids[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT

# No setting of a desktop session comes in, so each run gives the same pixels.
unset GTK_THEME GTK2_RC_FILES XCURSOR_THEME XCURSOR_SIZE XCURSOR_PATH \
  DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE \
  LD_PRELOAD LD_LIBRARY_PATH FAKETIME QT_QPA_PLATFORMTHEME QT_STYLE_OVERRIDE

# The home directory of the demo user: the files of Home Manager, and some
# folders and files for Thunar and ls.
export HOME=$work/demo
export XDG_CONFIG_HOME=$HOME/.config
export XDG_DATA_HOME=$HOME/.local/share
export XDG_STATE_HOME=$HOME/.local/state
export XDG_CACHE_HOME=$work/cache
export XDG_RUNTIME_DIR=$work/run
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR"
cp -rL --no-preserve=mode "$HOME_FILES/." "$HOME/"
mkdir -p "$HOME"/{Documents,Downloads,Music,Pictures,Projects}
printf 'Buy milk\n' >"$HOME/notes.txt"
printf '# To do\n' >"$HOME/todo.md"

# The packages of the user: the theme, its icon themes, kwm and kwim.
export XDG_DATA_DIRS=$HOME_PATH/share:$EXTRA_DATA_DIRS
export PATH=$HOME_PATH/bin:$PATH

# The GTK programs read the theme, the icon theme and the font from GSettings,
# as in a session: the values that Home Manager writes into dconf. The keyfile
# backend of GLib reads them from a file, without dconf.
export GSETTINGS_BACKEND=keyfile
mkdir -p "$XDG_CONFIG_HOME/glib-2.0/settings"
cp "$GSETTINGS_KEYFILE" "$XDG_CONFIG_HOME/glib-2.0/settings/keyfile"
export GTK_A11Y=none
export GTK_USE_PORTAL=0
export NO_AT_BRIDGE=1
export TZ=UTC
export LANG=C.UTF-8

# One output of 1280x720 pixels, without a GPU and without input devices.
export WLR_BACKENDS=headless
export WLR_RENDERER=pixman
export WLR_HEADLESS_OUTPUTS=1
export WLR_LIBINPUT_NO_DEVICES=1

# The session bus of the tray and of Xfconf.
dbus-daemon --config-file="$DBUS_SESSION_CONF" --fork \
  --print-address=3 --print-pid=4 3>"$work/bus-address" 4>"$work/bus-pid"
pids+=("$(cat "$work/bus-pid")")
DBUS_SESSION_BUS_ADDRESS=$(cat "$work/bus-address")
export DBUS_SESSION_BUS_ADDRESS

# Thunar without the status bar: it shows the free space of the disk.
mkdir -p "$XDG_CONFIG_HOME/xfce4/xfconf/xfce-perchannel-xml"
cat >"$XDG_CONFIG_HOME/xfce4/xfconf/xfce-perchannel-xml/thunar.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<channel name="thunar" version="1.0">
  <property name="last-statusbar-visible" type="bool" value="false"/>
  <property name="last-view" type="string" value="ThunarIconView"/>
</channel>
XML

# kwm gets a fixed clock, and fixed values for the CPU, the memory and the
# disk. The clock starts at 12:00:00 and runs, but the bar shows only the
# minutes. The other programs have the real clock.
# kwm reads the scheme from GTK_THEME here. In a session, it reads the scheme
# from the settings portal.
cat >"$work/kwm" <<KWM
#!/bin/sh
export GTK_THEME="$THEME_NAME"
export LD_PRELOAD="$FAKETIME_LIB:$PRELOAD_LIB"
export FAKETIME="@2026-10-04 12:00:00"
export FAKETIME_DONT_FAKE_MONOTONIC=1
exec kwm
KWM
chmod +x "$work/kwm"

river -no-xwayland -c "$work/kwm" >"$work/river.log" 2>&1 &
pids+=("$!")

# river makes the socket of the Wayland display.
for _ in $(seq 100); do
  socket=$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit)
  [ -n "$socket" ] && break
  sleep 0.1
done
if [ -z "${socket:-}" ]; then
  echo "mzde-screenshot: river did not start" >&2
  cat "$work/river.log" >&2
  exit 1
fi
WAYLAND_DISPLAY=$(basename "$socket")
export WAYLAND_DISPLAY
sleep 1

# Three tray items, with the icons of the icon theme of the tray.
python3 "$TRAY_SCRIPT" audio-volume-high network-wireless-signal-excellent bluetooth-active &
pids+=("$!")

# Thunar first: kwm puts the new window in the master area, so Ghostty comes
# to the left and has the focus.
thunar "$HOME" >"$work/thunar.log" 2>&1 &
pids+=("$!")
sleep 2

# Ghostty with the colors of mz-term-colors for the accent color of mzde. The
# shell prints a fixed text and waits.
mkdir -p "$XDG_CONFIG_HOME/ghostty"
cat >"$XDG_CONFIG_HOME/ghostty/config" <<GHOSTTY
theme = $GHOSTTY_THEME
window-decoration = none
title = Ghostty
font-size = 10
cursor-style-blink = false
shell-integration = none
confirm-close-surface = false
gtk-single-instance = false
GHOSTTY
cat >"$work/shell.sh" <<'SHELL'
cd "$HOME"
prompt() { printf '\033[1;32mdemo@mzde\033[0m \033[1;34m~\033[0m $ %s\n' "$1"; }
prompt ls
ls --color=always
prompt colors
for row in 0 8; do
  for i in $(seq "$row" $((row + 7))); do
    printf '\033[48;5;%sm    \033[0m ' "$i"
  done
  printf '\n'
done
printf '\033[1;32mdemo@mzde\033[0m \033[1;34m~\033[0m $ '
exec sleep infinity
SHELL
__EGL_VENDOR_LIBRARY_FILENAMES=$MESA_EGL_VENDOR LIBGL_DRIVERS_PATH=$MESA_DRIVERS \
  LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe \
  ghostty -e sh "$work/shell.sh" >"$work/ghostty.log" 2>&1 &
pids+=("$!")

# The screenshot when two screenshots in a row are equal: the windows have
# drawn, the widgets have updated, and the tray has its icons.
sleep 3
grim "$work/previous.png"
stable=
for _ in $(seq 30); do
  sleep 1
  grim "$work/current.png"
  if cmp -s "$work/previous.png" "$work/current.png"; then
    stable=1
    break
  fi
  mv "$work/current.png" "$work/previous.png"
done
if [ -z "$stable" ]; then
  echo "mzde-screenshot: the screen did not become stable" >&2
  tail -n 20 "$work/river.log" "$work/thunar.log" "$work/ghostty.log" >&2
  exit 1
fi

install -m 644 "$work/current.png" "$out/$variant.png"
echo "mzde-screenshot: wrote $out/$variant.png"
