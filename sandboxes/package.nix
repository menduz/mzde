# The `sandbox` command (src/). One program with the names sandbox,
# sandbox-init and sandbox-backend-{bwrap,gvisor}. The build gives the
# absolute paths of the programs that it starts (refer to src/tools.rs), so
# the PATH of the caller does not select them.
{
  lib,
  rustPlatform,
  bubblewrap,
  dbus,
  gnupg,
  gvisor,
  iproute2,
  iptables,
  libnotify,
  nftables,
  slirp4netns,
  tun2socks,
  util-linux,
  xwayland-satellite,
}:
rustPlatform.buildRustPackage {
  pname = "sandbox";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./tests
    ];
  };
  cargoLock.lockFile = ./Cargo.lock;

  env = {
    SANDBOX_TOOL_BWRAP = lib.getExe' bubblewrap "bwrap";
    SANDBOX_TOOL_RUNSC = lib.getExe' gvisor "runsc";
    SANDBOX_TOOL_SCRIPT = lib.getExe' util-linux "script";
    SANDBOX_TOOL_SLIRP4NETNS = lib.getExe slirp4netns;
    SANDBOX_TOOL_TUN2SOCKS = lib.getExe tun2socks;
    SANDBOX_TOOL_IP = lib.getExe' iproute2 "ip";
    SANDBOX_TOOL_IPTABLES = lib.getExe' iptables "iptables";
    SANDBOX_TOOL_IP6TABLES = lib.getExe' iptables "ip6tables";
    SANDBOX_TOOL_NFT = lib.getExe' nftables "nft";
    SANDBOX_TOOL_GPGCONF = lib.getExe' gnupg "gpgconf";
    SANDBOX_TOOL_DBUS_DAEMON = lib.getExe' dbus "dbus-daemon";
    # The configuration of the package, not /etc: a system without dbus has
    # no /etc/dbus-1/session.conf.
    SANDBOX_TOOL_DBUS_SESSION_CONF = "${dbus.out}/share/dbus-1/session.conf";
    SANDBOX_TOOL_XWAYLAND_SATELLITE = lib.getExe xwayland-satellite;
    SANDBOX_TOOL_NOTIFY_SEND = lib.getExe' libnotify "notify-send";
  };

  # The program selects its function by the name of the link.
  postInstall = ''
    for name in sandbox-init sandbox-backend-bwrap sandbox-backend-gvisor; do
      ln -s sandbox "$out/bin/$name"
    done
  '';

  meta = {
    description = "Run programs in named sandboxes";
    mainProgram = "sandbox";
    platforms = lib.platforms.linux;
  };
}
