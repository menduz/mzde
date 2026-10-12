{
  pkgs,
  nixpkgs,
  home-manager,
}:
let
  inherit (pkgs) lib;
  interface = "org.freedesktop.impl.portal.Inhibit";
  portal = pkgs.callPackage ./package.nix { };

  nixos =
    extra:
    (nixpkgs.lib.nixosSystem {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ./modules/nixos.nix
        {
          boot.isContainer = true;
          system.stateVersion = "26.05";
        }
        extra
      ];
    }).config;

  home =
    extra:
    (home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        ./modules/home-manager.nix
        {
          home.username = "portal-test";
          home.homeDirectory = "/home/portal-test";
          home.stateVersion = "26.05";
        }
        extra
      ];
    }).config;

  enabled = {
    services.kwm-portal.enable = true;
    xdg.portal.extraPortals = [ pkgs.xdg-desktop-portal-gtk ];
    xdg.portal.config.river = {
      default = "gtk";
      "org.freedesktop.impl.portal.ScreenCast" = "wlr";
    };
  };
  custom = {
    services.kwm-portal = {
      enable = true;
      # A sentinel package verifies that the supplied value is used everywhere.
      package = pkgs.hello;
      desktopNames = [
        "custom"
        "common"
      ];
    };
    xdg.portal.config.custom.${interface} = "none";
  };
  unrouted = {
    services.kwm-portal.enable = true;
    services.kwm-portal.desktopNames = [ ];
    xdg.portal.config.common.default = "gtk";
  };

  checkCommon =
    evaluate:
    let
      off = evaluate { };
      on = evaluate enabled;
      overridden = evaluate custom;
      manual = evaluate unrouted;
    in
    assert !off.xdg.portal.enable;
    assert off.xdg.portal.extraPortals == [ ];
    assert off.xdg.portal.config == { };
    assert on.xdg.portal.enable;
    assert builtins.elem portal on.xdg.portal.extraPortals;
    assert builtins.elem pkgs.xdg-desktop-portal-gtk on.xdg.portal.extraPortals;
    assert on.xdg.portal.config.river.${interface} == "kwm";
    assert on.xdg.portal.config.river.default == "gtk";
    assert on.xdg.portal.config.river."org.freedesktop.impl.portal.ScreenCast" == "wlr";
    assert overridden.xdg.portal.extraPortals == [ pkgs.hello ];
    assert overridden.xdg.portal.config.custom.${interface} == "none";
    assert overridden.xdg.portal.config.common.${interface} == "kwm";
    assert !(overridden.xdg.portal.config ? river);
    assert manual.xdg.portal.enable;
    assert manual.xdg.portal.config == { common.default = "gtk"; };
    assert lib.all (a: a.assertion) on.assertions;
    true;

  nixosOn = nixos enabled;
  homeOn = home enabled;
  routingLine = "${interface}=kwm";
in
{
  nixos-module =
    assert checkCommon nixos;
    assert nixosOn.services.dbus.enable;
    assert builtins.elem portal nixosOn.services.dbus.packages;
    assert builtins.elem portal nixosOn.systemd.packages;
    assert builtins.elem portal nixosOn.environment.systemPackages;
    assert lib.hasInfix routingLine
      nixosOn.environment.etc."xdg/xdg-desktop-portal/river-portals.conf".text;
    pkgs.runCommand "kwm-portal-nixos-module-check" { } "touch $out";

  home-manager-module =
    assert checkCommon home;
    assert (home { }).systemd.user.packages == [ ];
    assert builtins.elem portal homeOn.home.packages;
    assert builtins.elem portal homeOn.systemd.user.packages;
    assert builtins.elem pkgs.hello (home custom).systemd.user.packages;
    assert homeOn.xdg.dataFile ? "systemd/user";
    assert lib.hasInfix routingLine homeOn.xdg.configFile."xdg-desktop-portal/river-portals.conf".text;
    pkgs.runCommand "kwm-portal-home-manager-module-check" { } "touch $out";
}
