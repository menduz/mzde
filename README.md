# mzde

A desktop for NixOS: [river](https://codeberg.org/river/river) (the main
branch, with patches) and the [kwm](https://github.com/menduz/kwm) window
manager, the [win-classic](https://github.com/menduz/win-classic-theme) theme
for GTK, kwm and rofi, and the [waylock](https://github.com/menduz/waylock)
screen locker.

![mzde with the dark scheme](screenshots/dark.png)
![mzde with the light scheme](screenshots/light.png)

- The kwm bar: tags, layout, title, memory, clock, CPU, busy disks, privacy
  (microphone and camera), sleep inhibitors, media, docker, battery, disk
  usage, the notifications of dunst, and the system tray
  (`nix/widgets.nix`).
- Key bindings with an overlay: hold Super for 2 seconds.
- rofi menus as modal dialogs in the style of the theme (`mz-rofi`).
- swayidle locks the screen, turns off the outputs and suspends.
- kwm-portal: programs can stop the idle timers (the Inhibit portal).
- `toggle-theme` (Super+Shift O) changes between the light and the dark
  scheme without a rebuild.
- Screenshots with `scrn` (Super+Alt 1) and `slurpscrn` (Super+Alt 4), the
  wlr and GTK portals (screen sharing with a rofi chooser), polkit.

## Use

mzde is a NixOS module. It needs the NixOS module of Home Manager: it gives
the user part of the desktop to each Home Manager user.

```nix
{
  inputs.mzde = {
    url = "git+https://github.com/menduz/mzde.git?ref=main";
    inputs.nixpkgs.follows = "nixpkgs";
    inputs.home-manager.follows = "home-manager";
  };

  outputs = { nixpkgs, home-manager, mzde, ... }: {
    nixosConfigurations.box = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        home-manager.nixosModules.home-manager
        mzde.nixosModules.default
        {
          mzde.enable = true;
          mzde.keymap.layout = "us";
          mzde.bindings."Super Return" = "ghostty";
        }
      ];
    };
  };
}
```

The display manager shows the session "river". Start the session programs
with `mzde.sessionTarget` (`river-session.target`).

## Options

All options are in `nix/options.nix`.

- `mzde.bindings`, `mzde.floatRules`, `mzde.startupCommands`: key bindings
  that run commands, windows that float, programs to start.
- `mzde.terminal`, `mzde.keymap`, `mzde.keyRepeat`, `mzde.naturalScroll`.
- `mzde.bar.center`, `mzde.bar.right`, `mzde.bar.end`: more widgets.
  `mzde.lib.scriptWidget` makes a widget from a script.
- `mzde.colors`, `mzde.kwm.settings`: colors, and other values for the
  configuration of kwm.
- `mzde.lock.*`: idle times, and a command after the unlock.
- `mzde.rofi.plugins`; `mzde.rofi.package` is the `mz-rofi` command.
- `mzde.toggleTheme` is the `toggle-theme` command.
- `programs.win-classic-theme.*`: the theme. Refer to win-classic-theme.

## Development

The inputs `river`, `kwm`, `waylock`, `win-classic-theme` and
`mz-term-colors` are the forks on GitHub. To build with local checkouts, use
`--override-input`, for example `--override-input kwm git+file:///path/kwm`.
From a flake that has mzde as an input, use `--override-input mzde/kwm ...`.

`nix run .#screenshots` writes `screenshots/dark.png` and
`screenshots/light.png`: river with the headless backend, kwm with the
configuration of a demo system (`dev/demo.nix`), Thunar and Ghostty (Mesa
draws for Ghostty with the processor). The clock, the CPU, the memory and the
disk always have the same values, so the images change only when the desktop
changes. `nix build .#screenshots` only builds them; the build is
reproducible.

`nix build .#checks.x86_64-linux.lock-prompt` tests the patches of river and
waylock: the password prompt stays in the center after a change of the output.
