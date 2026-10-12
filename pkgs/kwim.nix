# Builds kwim, the input manager of kwm. kwim sets the libinput and xkb options
# of the input devices through river-input-management-v1. kwm starts kwim, and
# kwim reads the input rules from the kwm configuration.
# Refer to https://github.com/kewuaa/kwim.
#
# After an update of the kwim input, the hash of the Zig dependencies can
# change. Then the build fails and shows the new hash. Put that hash below.
{
  lib,
  callPackage,
  stdenv,
  zig_0_16,
  pkg-config,
  wayland,
  wayland-protocols,
  wayland-scanner,
  libxkbcommon,
  src,
}:
let
  zigSystemDeps = callPackage ./zig-system-deps.nix { };
in
stdenv.mkDerivation (finalAttrs: {
  pname = "kwim";
  version = "0-unstable-${src.lastModifiedDate or "0"}";
  inherit src;

  deps = zigSystemDeps {
    inherit (finalAttrs) pname version src;
    hash = "sha256-rOZZu/Y/rZ7who3hl1qIBHXZtRP7s4FXo0+LNnM6dYo=";
  };

  strictDeps = true;

  nativeBuildInputs = [
    pkg-config
    wayland-scanner
    zig_0_16
  ];

  buildInputs = [
    libxkbcommon
    wayland
    wayland-protocols
    # build.zig asks pkg-config for the protocol directory of wayland-scanner.
    wayland-scanner
  ];

  zigBuildFlags = [
    "--system"
    "${finalAttrs.deps}"
    "-Dversion-string=${finalAttrs.version}"
  ];

  meta = {
    description = "Input manager for river, from the kwm project";
    homepage = "https://github.com/kewuaa/kwim";
    license = lib.licenses.gpl3Only;
    mainProgram = "kwim";
    platforms = lib.platforms.linux;
  };
})
