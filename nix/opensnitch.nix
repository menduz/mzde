# The OpenSnitch fork (the input opensnitch: menduz/opensnitch, branch
# fork), as an overlay: the daemon, the UI and the eBPF modules of nixpkgs,
# built from the fork with the nixpkgs of the system. The sandboxes need the
# fork for their own daemons (programs.sandboxes.environments.<name>.opensnitch):
# the netfilter queue in a user namespace, the name of the node, and the
# sandbox of each process from its cgroup (the rule operand process.sandbox).
#
# The argument is the inputs of flake.nix.
inputs: final: prev: {
  opensnitch = prev.opensnitch.overrideAttrs (old: {
    # A local checkout (--override-input mzde/opensnitch) with uncommitted
    # changes has no revision.
    version = "1.9.0-fork-${inputs.opensnitch.shortRev or "local"}";
    # opensnitch-ui and opensnitch-ebpf use src.name to find their sourceRoot.
    src = inputs.opensnitch // {
      name = "source";
    };
    vendorHash = "sha256-bxZoaX7Y8xKsqnfqmP50KDfks41xPTn4pf2is3mLccs=";
  });
}
