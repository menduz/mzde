# The waylock password prompt after a change of the output. Refer to
# lock-prompt.sh. `nix build .#checks.x86_64-linux.lock-prompt` runs it in the
# sandbox.
{
  runCommand,
  makeFontsConf,
  river,
  kwm,
  waylock,
  wlr-randr,
  grim,
  python3,
  coreutils,
  findutils,
  terminus_font,
}:
runCommand "lock-prompt"
  {
    nativeBuildInputs = [
      river
      kwm
      waylock
      wlr-randr
      grim
      (python3.withPackages (ps: [ ps.pillow ]))
      coreutils
      findutils
    ];
    # Terminus, the default font of the prompt.
    FONTCONFIG_FILE = makeFontsConf { fontDirectories = [ terminus_font ]; };
    PROMPT_CENTER = ./prompt_center.py;
    meta.description = "The waylock prompt stays in the center after a change of the output";
  }
  ''
    bash ${./lock-prompt.sh}
    touch "$out"
  ''
