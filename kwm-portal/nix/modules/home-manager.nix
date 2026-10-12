{ config, lib, ... }:
{
  imports = [ ./common.nix ];

  # Home Manager's xdg.portal module installs extraPortals in the profile.
  # Register the activation service's systemd unit in the user's data dir too.
  config = lib.mkIf config.services.kwm-portal.enable {
    systemd.user.packages = [ config.services.kwm-portal.package ];
  };
}
