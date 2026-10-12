{ config, lib, ... }:
{
  imports = [ ./common.nix ];

  # NixOS's xdg.portal module registers extraPortals with both D-Bus and
  # systemd and adds their portal descriptors to the system profile.
  config = lib.mkIf config.services.kwm-portal.enable {
    services.dbus.enable = true;
  };
}
