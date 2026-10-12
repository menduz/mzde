{
  lib,
  stdenv,
  zig_0_16,
  pkg-config,
  systemd,
  dbus,
  ripgrep,
  python3,
}:
stdenv.mkDerivation {
  pname = "kwm-portal";
  version = "0.2.0";
  src = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      lib.cleanSourceFilter path type
      && !(builtins.elem (baseNameOf path) [
        ".zig-cache"
        "zig-out"
        "build"
      ]);
  };
  nativeBuildInputs = [
    zig_0_16
    pkg-config
  ];
  buildInputs = [ systemd ];
  nativeCheckInputs = [
    dbus
    systemd
    ripgrep
    (python3.withPackages (ps: [ ps.dbus-next ]))
  ];
  dontConfigure = true;
  postPatch = ''
    patchShebangs tests/service.sh
  '';
  buildPhase = ''
    runHook preBuild
    export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-cache"
    zig build -Doptimize=ReleaseSafe --prefix "$out"
    runHook postBuild
  '';
  doCheck = true;
  checkPhase = ''
    runHook preCheck
    zig build test -Doptimize=ReleaseSafe --summary all
    tests/service.sh "$out/bin/kwm-portal"
    dbus-run-session --config-file=tests/session.conf -- python3 tests/inhibit.py "$out/bin/kwm-portal"
    runHook postCheck
  '';
  installPhase = ''
    runHook preInstall
    install -Dm644 data/kwm.portal "$out/share/xdg-desktop-portal/portals/kwm.portal"
    substitute data/org.freedesktop.impl.portal.desktop.kwm.service.in "$TMPDIR/dbus.service" --subst-var out
    install -Dm644 "$TMPDIR/dbus.service" "$out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.kwm.service"
    substitute data/kwm-portal.service.in "$TMPDIR/user.service" --subst-var out
    install -Dm644 "$TMPDIR/user.service" "$out/lib/systemd/user/kwm-portal.service"
    runHook postInstall
  '';
  meta = {
    description = "Desktop portal backend forwarding inhibition to systemd-logind";
    mainProgram = "kwm-portal";
    platforms = lib.platforms.linux;
  };
}
