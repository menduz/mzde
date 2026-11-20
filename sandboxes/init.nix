# sandbox-init: the first program in a sandbox. It starts the private session
# bus and the private X server of a run. Refer to init.sh.
{
  writeShellApplication,
  coreutils,
  dbus,
  xwayland-satellite,
}:
writeShellApplication {
  name = "sandbox-init";
  runtimeInputs = [
    coreutils
    dbus
    xwayland-satellite
  ];
  # The configuration of the package, not /etc: a system without dbus has no
  # /etc/dbus-1/session.conf.
  text = ''
    DBUS_SESSION_CONF=${dbus.out}/share/dbus-1/session.conf
  ''
  + builtins.readFile ./init.sh;
}
