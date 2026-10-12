{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.kwm-portal;
in
{
  options.services.kwm-portal = {
    enable = lib.mkEnableOption "the kwm Inhibit portal backend";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ../package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage kwm-portal/nix/package.nix { }";
      description = "The kwm-portal package to install and activate.";
    };

    desktopNames = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ "river" ];
      example = [
        "river"
        "common"
      ];
      description = ''
        Desktop configuration names under xdg.portal.config for which kwm
        should handle Inhibit. Use lowercase XDG_CURRENT_DESKTOP names, or
        "common" for the fallback portals.conf. An empty list installs the
        backend without choosing its routing.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    xdg.portal = {
      enable = true;
      extraPortals = [ cfg.package ];
      config = lib.genAttrs cfg.desktopNames (_: {
        "org.freedesktop.impl.portal.Inhibit" = lib.mkDefault "kwm";
      });
    };
  };
}
