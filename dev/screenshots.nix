# The screenshots of mzde: river with the headless backend of wlroots, kwm
# with the configuration of the module, Ghostty and Thunar, in the dark and in
# the light scheme of the theme. Refer to screenshot.sh.
#
# The demo system (demo.nix) gives the files of the home directory, so the
# screenshots show the configuration that a user of mzde gets. The bar always
# shows the same clock, CPU, memory and disk values, so the images change only
# when the desktop changes.
{
  pkgs,
  lib,
  inputs,
  nixosModule,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;
  packages = import ../pkgs { inherit pkgs inputs; };
  termColors = import "${inputs.mz-term-colors}/lib" { inherit pkgs; };

  # The configuration of the demo system with one variant of the theme.
  demo =
    variant:
    (inputs.nixpkgs.lib.nixosSystem {
      inherit system;
      modules = [
        inputs.home-manager.nixosModules.home-manager
        nixosModule
        (import ./demo.nix variant)
      ];
    }).config;

  # Fixed values for the CPU, memory, disk and battery widgets of kwm. The
  # library is in the kwm source. Refer to nix/screenshot-preload.c in kwm.
  preload = pkgs.runCommandCC "mzde-screenshot-preload" { } ''
    mkdir -p "$out/lib"
    $CC -shared -fPIC -O2 -Wall -Wextra -o "$out/lib/screenshot-preload.so" \
      ${inputs.kwm}/nix/screenshot-preload.c -ldl
  '';

  # The fonts of the demo system: Inter for GTK, Terminus for the bar, the
  # Nerd Font symbols for the icons of the widgets, DejaVu for the other
  # glyphs. Ghostty has its own font.
  fontsConf = pkgs.writeText "mzde-screenshot-fonts.conf" ''
    <?xml version="1.0"?>
    <!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd">
    <fontconfig>
      <include>${
        pkgs.makeFontsConf {
          fontDirectories = [
            pkgs.inter
            pkgs.terminus_font
            pkgs.nerd-fonts.symbols-only
            pkgs.dejavu_fonts
          ];
        }
      }</include>
      <alias>
        <family>sans-serif</family>
        <prefer><family>Inter</family></prefer>
      </alias>
      <alias>
        <family>monospace</family>
        <prefer>
          <family>DejaVu Sans Mono</family>
          <family>Symbols Nerd Font Mono</family>
        </prefer>
      </alias>
    </fontconfig>
  '';

  # The GSettings of a user, in the format of the keyfile backend of GLib: the
  # values that Home Manager writes into dconf. GTK on Wayland reads the font,
  # the icon theme and the GTK theme from GSettings, not from settings.ini.
  gsettingsKeyfile =
    variant: user:
    pkgs.writeText "mzde-screenshot-${variant}-gsettings" (
      lib.generators.toINI {
        mkKeyValue = key: value: "${key}=${toString (inputs.home-manager.lib.hm.gvariant.mkValue value)}";
      } user.dconf.settings
    );

  # The settings of one screenshot.
  variantEnv =
    variant:
    let
      config = demo variant;
      user = config.home-manager.users.demo;
    in
    pkgs.writeText "mzde-screenshot-${variant}.env" ''
      HOME_FILES=${user.home-files}
      GSETTINGS_KEYFILE=${gsettingsKeyfile variant user}
      HOME_PATH=${user.home.path}
      THEME_NAME=${config.programs.win-classic-theme.theme.variantName}
      GHOSTTY_THEME=${
        termColors.ghosttyTheme {
          color = config.mzde.colors.border.focus;
          inherit variant;
        }
      }
    '';

  screenshot = pkgs.writeShellApplication {
    name = "mzde-screenshot";
    runtimeInputs = [
      packages.river
      pkgs.ghostty
      pkgs.thunar
      pkgs.xfconf
      pkgs.grim
      pkgs.dbus
      (pkgs.python3.withPackages (ps: [ ps.dbus-next ]))
      pkgs.coreutils
      pkgs.findutils
    ];
    text = ''
      export FONTCONFIG_FILE=${fontsConf}
      export DBUS_SESSION_CONF=${pkgs.dbus}/share/dbus-1/session.conf
      # Xfconf keeps the settings of Thunar. The bus starts it.
      export EXTRA_DATA_DIRS=${pkgs.xfconf}/share:${pkgs.thunar}/share
      export FAKETIME_LIB=${pkgs.libfaketime}/lib/libfaketime.so.1
      export PRELOAD_LIB=${preload}/lib/screenshot-preload.so
      export TRAY_SCRIPT=${inputs.kwm}/nix/screenshot-tray.py
      # Ghostty needs OpenGL. Mesa draws with the processor (llvmpipe) into
      # shared memory, so no GPU is necessary.
      export MESA_EGL_VENDOR=${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json
      export MESA_DRIVERS=${pkgs.mesa}/lib/dri
      exec bash ${./screenshot.sh} "$@"
    '';
  };

  variants = [
    "dark"
    "light"
  ];
in
{
  inherit screenshot;

  screenshots =
    pkgs.runCommand "mzde-screenshots"
      {
        nativeBuildInputs = [ screenshot ];
        meta.description = "Screenshots of mzde";
      }
      ''
        ${lib.concatMapStrings (variant: ''
          mzde-screenshot "$out" ${variant} ${variantEnv variant}
        '') variants}
      '';

  # `nix run .#screenshots` builds the screenshots and puts them in
  # screenshots/ of the working tree.
  update = pkgs.writeShellApplication {
    name = "update-screenshots";
    text = ''
      if [ ! -e ./flake.nix ] || [ ! -e ./dev/screenshots.nix ]; then
        echo "update-screenshots: run this in the directory of mzde" >&2
        exit 1
      fi
      if ! out=$(nix build --no-link --print-out-paths .#screenshots); then
        echo "update-screenshots: the build failed. Nix reads the files that" >&2
        echo "Git tracks, so a new file needs 'git add' first." >&2
        exit 1
      fi
      mkdir -p screenshots
      install -m 644 "$out"/*.png screenshots/
      echo "update-screenshots: wrote"
      ls -1 screenshots/
    '';
  };
}
