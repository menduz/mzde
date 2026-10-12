# Builds waylock, the screen locker of river. The source is the fork
# menduz/waylock, the waylock input. Upstream is
# https://codeberg.org/ifreund/waylock. The fork shows a password prompt in
# the center of each output, drawn with fcft and pixman.
#
# After an update of the waylock input, the hash of the Zig dependencies can
# change. Then the build fails and shows the new hash. Put that hash below.
{
  lib,
  callPackage,
  stdenv,
  zig_0_16,
  pkg-config,
  scdoc,
  wayland,
  wayland-protocols,
  wayland-scanner,
  libxkbcommon,
  pam,
  fcft,
  pixman,
  src,
}:
let
  zigSystemDeps = callPackage ./zig-system-deps.nix { };
in
stdenv.mkDerivation (finalAttrs: {
  pname = "waylock";
  version = "0-unstable-${src.lastModifiedDate or "0"}";
  inherit src;

  deps = zigSystemDeps {
    inherit (finalAttrs) pname version src;
    hash = "sha256-3KV4mhxjqYocIWtdY6tg0A9njg25R5bL7/PPf2MvA1w=";
  };

  strictDeps = true;

  nativeBuildInputs = [
    pkg-config
    scdoc
    wayland-scanner
    zig_0_16
  ];

  buildInputs = [
    wayland
    # The scanner of zig-wayland asks pkg-config for the protocol directory of
    # wayland-scanner.
    wayland-scanner
    wayland-protocols
    libxkbcommon
    pam
    fcft
    pixman
  ];

  # NixOS has no system-auth PAM service. security.pam.services.waylock in
  # nix/lock.nix gives the service of the system.
  preBuild = ''
    substituteInPlace pam.d/waylock --replace-fail "system-auth" "login"
  '';

  zigBuildFlags = [
    "-Dman-pages"
    "--system"
    "${finalAttrs.deps}"
  ];

  # `zig build test`, with the dependencies of the build.
  doCheck = true;
  zigCheckFlags = [
    "--system"
    "${finalAttrs.deps}"
  ];

  meta = {
    description = "Small screenlocker for Wayland compositors";
    homepage = "https://github.com/menduz/waylock";
    license = lib.licenses.isc;
    mainProgram = "waylock";
    platforms = lib.platforms.linux;
  };
})
