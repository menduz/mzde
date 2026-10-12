#!/usr/bin/env bash
set -euo pipefail

if [[ ${KWM_PORTAL_TEST_BUS:-} != 1 ]]; then
  exec env KWM_PORTAL_TEST_BUS=1 dbus-run-session \
    --config-file="$(dirname "$0")/session.conf" -- "$0" "$@"
fi

export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"
portal=$1
name=org.freedesktop.impl.portal.desktop.kwm
work=$(mktemp -d)
pid=
cleanup() {
  if [[ -n "$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT

"$portal" >"$work/service.log" 2>&1 &
pid=$!
ready=false
for ((i = 0; i < 100; i++)); do
  if busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
      org.freedesktop.DBus NameHasOwner s "$name" | rg -q 'true'; then
    ready=true
    break
  fi
  sleep 0.05
done
if [[ "$ready" != true ]]; then
  cat "$work/service.log" >&2
  exit 1
fi

# A second instance must fail without replacing the first one.
if "$portal" >"$work/duplicate.log" 2>&1; then
  echo 'Duplicate service unexpectedly succeeded' >&2
  exit 1
fi
kill -0 "$pid"
kill -TERM "$pid"
wait "$pid"
pid=
busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
  org.freedesktop.DBus NameHasOwner s "$name" | rg -q 'false'

# A clean exit must leave the name available for a new instance.
"$portal" >"$work/restart.log" 2>&1 &
pid=$!
for ((i = 0; i < 100; i++)); do
  if busctl --user call org.freedesktop.DBus /org/freedesktop/DBus \
      org.freedesktop.DBus NameHasOwner s "$name" | rg -q 'true'; then
    kill -INT "$pid"
    wait "$pid"
    pid=
    exit 0
  fi
  sleep 0.05
done
cat "$work/restart.log" >&2
exit 1
