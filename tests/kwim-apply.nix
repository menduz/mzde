# kwim applies its settings when river is slow. Refer to kwim-apply.sh.
# `nix build .#checks.x86_64-linux.kwim-apply` runs it in the sandbox.
{
  runCommand,
  river,
  kwm,
  kwim,
  wtype,
  python3,
  coreutils,
  gnugrep,
}:
runCommand "kwim-apply"
  {
    nativeBuildInputs = [
      river
      kwm
      kwim
      wtype
      (python3.withPackages (ps: [ ps.pywayland ]))
      coreutils
      gnugrep
    ];
    REPEAT_INFO = ./repeat_info.py;
    meta.description = "kwim applies its settings when river is slow";
  }
  ''
    bash ${./kwim-apply.sh}
    touch "$out"
  ''
