# The `sandbox` command (the frontend). It reads the sandboxes from /etc/sandboxes/config.json,
# or from the file in SANDBOX_CONFIG. Refer to module.nix.
{
  writeShellApplication,
  coreutils,
  gawk,
  getent,
  gnused,
  iproute2,
  iptables,
  jq,
  nftables,
  slirp4netns,
  tun2socks,
  util-linux,
}:
writeShellApplication {
  name = "sandbox";
  runtimeInputs = [
    coreutils
    gawk
    getent
    gnused
    iproute2
    iptables
    jq
    nftables
    slirp4netns
    tun2socks
    util-linux
  ];
  # systemd-run, systemctl and gpgconf come from the system, because they
  # must agree with the services that run.
  inheritPath = true;
  text = builtins.readFile ./sandbox.sh;
  meta = {
    description = "Run programs in named sandboxes";
    mainProgram = "sandbox";
  };
}
