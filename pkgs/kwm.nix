# Builds kwm, a window manager for river 0.4 and later. kwm is DWM-like, with
# tags and a tile layout. The source is the fork menduz/kwm, the kwm
# input. Upstream is https://github.com/kewuaa/kwm.
#
# After an update of the kwm input, the hash of the Zig dependencies can change.
# Then the build fails and shows the new hash. Put that hash below.
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
  fcft,
  pixman,
  systemdLibs,
  libspng,
  resvg,
  libghostty-vt,
  src,
}:
let
  zigSystemDeps = callPackage ./zig-system-deps.nix { };
in
stdenv.mkDerivation (finalAttrs: {
  pname = "kwm";
  version = "0-unstable-${src.lastModifiedDate or "0"}";
  inherit src;

  deps = zigSystemDeps {
    inherit (finalAttrs) pname version src;
    # fcft and pixman are lazy dependencies. Only the bar uses them.
    fetchAll = true;
    hash = "sha256-Lz/Wcy40rxN81n/mBj4YJVbyGOolHzSFZMs93T1h0oQ=";
  };

  strictDeps = true;

  nativeBuildInputs = [
    pkg-config
    wayland-scanner
    zig_0_16
  ];

  buildInputs = [
    fcft
    libxkbcommon
    pixman
    # sd-bus for the tray, and the icons of the tray.
    systemdLibs
    libspng
    resvg
    # The terminal of the overlay of the Super key.
    libghostty-vt
    wayland
    # build.zig asks pkg-config for the protocol directory of wayland-scanner.
    wayland-scanner
    wayland-protocols
  ];

  zigBuildFlags = [
    "--system"
    "${finalAttrs.deps}"
    "-Dversion-string=${finalAttrs.version}"
    # A solid background in the color of the desktop. It covers the
    # layer-shell surfaces on the bottom layer, for example a wallpaper.
    "-Dbackground=true"
    # kwm starts kwim to apply the input rules of the kwm configuration.
    "-Dkwim=true"
    # The tray in the bar (StatusNotifierItem).
    "-Dtray=true"
    # The overlay of the Super key: the key bindings.
    "-Doverlay=true"
  ];

  meta = {
    description = "DWM-like dynamic tiling window manager for river";
    homepage = "https://github.com/kewuaa/kwm";
    license = lib.licenses.gpl3Only;
    mainProgram = "kwm";
    platforms = lib.platforms.linux;
  };
})
