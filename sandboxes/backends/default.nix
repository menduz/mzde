# The built-in backends of `sandbox`: links to the `sandbox` program, which
# selects the backend by the name of the link (refer to src/backend.rs). A
# backend is a command with the interface
#
#   <backend> run PLAN.json    start the program of the plan
#   <backend> start PLAN.json  start the container of a sandbox
#   <backend> exec PLAN.json   start the program of the plan in that container
#   <backend> show PLAN.json   print the command, and do not start it
#
# Refer to README.md for the format of the plan.
{
  runCommand,
  sandbox,
}:
let
  backend =
    name:
    runCommand "sandbox-backend-${name}"
      {
        meta = {
          description = "The ${name} backend of sandbox";
          mainProgram = "sandbox-backend-${name}";
        };
      }
      ''
        mkdir -p $out/bin
        ln -s ${sandbox}/bin/sandbox-backend-${name} $out/bin/sandbox-backend-${name}
      '';
in
{
  bwrap = backend "bwrap";
  gvisor = backend "gvisor";
}
