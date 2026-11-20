# The backends of `sandbox`. A backend is a command with the interface
#
#   <backend> run PLAN.json    start the program of the plan
#   <backend> start PLAN.json  start the container of a sandbox
#   <backend> exec PLAN.json   start the program of the plan in that container
#   <backend> show PLAN.json   print the command, and do not start it
#
# Refer to README.md for the format of the plan.
{
  writeShellApplication,
  bubblewrap,
  coreutils,
  gvisor,
  jq,
  procps,
  util-linux,
}:
{
  bwrap = writeShellApplication {
    name = "sandbox-backend-bwrap";
    runtimeInputs = [
      bubblewrap
      jq
      procps
      util-linux
    ];
    text = builtins.readFile ./bwrap.sh;
    meta.description = "The bubblewrap backend of sandbox";
  };

  gvisor = writeShellApplication {
    name = "sandbox-backend-gvisor";
    runtimeInputs = [
      coreutils
      gvisor
      jq
      util-linux
    ];
    text = builtins.readFile ./gvisor.sh;
    meta.description = "The gVisor (runsc) backend of sandbox";
  };
}
