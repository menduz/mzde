# kwm-portal

A Zig desktop portal backend for kwm on River. It implements portal Inhibit
requests by acquiring systemd-logind inhibitor file descriptors. It runs as a
separate user service; kwm and kwim do not need changes.

## Supported inhibition

| Portal flag | logind `what` | Mode |
| --- | --- | --- |
| Suspend (`4`) | `sleep` | `block` |
| Idle (`8`) | `idle` | `block` |
| Suspend + idle (`12`) | `sleep:idle` | `block` |

The application ID becomes logind's `who`, and the portal reason becomes `why`.
Inhibitors are visible in `systemd-inhibit --list`, including kwm's existing
inhibitor widget. The service replies successfully only after logind grants the
inhibitor. Logind authorization errors are returned to the portal frontend.

Each request owns a duplicated, close-on-exec inhibitor fd. Closing the request,
losing the portal frontend, or stopping the service releases it. Acquisition is
asynchronous, so requests can be cancelled while logind is still responding.
Only the owner of `org.freedesktop.portal.Desktop` can create requests, and only
the creating connection can close them. The frontend handles application-level
permissions and closes backend requests when applications disappear.

**Idle means logind idle-action inhibition.** It does not inhibit River's Wayland
idle notifier or swayidle's independent screen-lock/display-off timers. Those
need a separate compositor bridge. Suspend inhibition also depends on the caller
of the sleep operation respecting logind inhibitors.

Logout (`1`) and user-switch (`2`) inhibition have no equivalent logind operation.
Requests containing either flag are rejected in full with `NotSupported`; they
are not translated to shutdown inhibition. Zero and unknown flags are invalid.
Session monitoring is not implemented: `CreateMonitor` returns failure (`2`),
and `QueryEndResponse` returns `NotSupported`. GlobalShortcuts is not implemented
or advertised yet.

## Build and test

Zig 0.16, pkg-config, libc and libsystemd are required. The flake pins the compiler
and dependencies. From this directory, use an explicit path flake while the
project is untracked inside the parent repository:

```sh
nix build path:.
nix flake check path:.
nix run path:. -- --help
```

For development:

```sh
nix develop path:.
zig build -Doptimize=Debug
zig build test --summary all
zig fmt build.zig src/main.zig
tests/service.sh "$PWD/zig-out/bin/kwm-portal"
dbus-run-session --config-file=tests/session.conf -- \
  python3 tests/inhibit.py "$PWD/zig-out/bin/kwm-portal"
```

Native Zig tests run without a D-Bus daemon and cover flag validation, frontend
ownership changes, close-on-exec fd retention, request destruction/handle reuse,
and allocation-failure cleanup. They use Zig's testing allocator to detect memory leaks. `nix flake check` runs
these tests as well as the integration tests.

Integration tests use a private bus for both user and system connections, with a mock logind
that passes real Unix file descriptors. They cover flag mapping, reason handling,
authorization failures, request ownership, duplicate handles, pending cancellation,
frontend disconnect/name loss, and release on shutdown. They never acquire host
inhibitors. Real logind policy and compositor idle behavior require host testing.

## NixOS and Home Manager modules

The flake exports `nixosModules.default` and `homeManagerModules.default`, with
`kwm-portal` aliases for both. Add the local project as an input to your flake:

```nix
inputs.kwm-portal = {
  url = "path:./kwm-portal";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

In a NixOS configuration module receiving `inputs` through `specialArgs`:

```nix
{ inputs, ... }:
{
  imports = [ inputs.kwm-portal.nixosModules.default ];
  services.kwm-portal.enable = true;
}
```

Or in a Home Manager configuration receiving `inputs` through `extraSpecialArgs`:

```nix
{ inputs, ... }:
{
  imports = [ inputs.kwm-portal.homeManagerModules.default ];
  services.kwm-portal.enable = true;
}
```

Without flake inputs, import `nix/modules/nixos.nix` or
`nix/modules/home-manager.nix` directly from this project. Choose one integration
for a given user session. Standalone Home Manager requires a Linux host with
systemd-logind and a working systemd user/D-Bus session.

Both modules enable `xdg.portal`, add the backend to `xdg.portal.extraPortals`,
and default River's Inhibit route to `kwm`. NixOS registers the package with D-Bus
and systemd through its portal module; Home Manager additionally registers the
packaged user unit with `systemd.user.packages`. D-Bus starts the service on
request; no login-time startup target is added. Existing portal backends and
other interface routes are preserved.

| Option | Default | Purpose |
| --- | --- | --- |
| `services.kwm-portal.enable` | `false` | Enable the backend and routing. |
| `services.kwm-portal.package` | Project package | Override the package used everywhere. |
| `services.kwm-portal.desktopNames` | `[ "river" ]` | Desktop configurations to route; `"common"` selects the fallback configuration. |

Set `desktopNames = [ ];` to install the backend and manage routing yourself.
Routes use `lib.mkDefault`, so explicit `xdg.portal.config` values take precedence.
Continue configuring GTK/wlr backends for interfaces such as file dialogs and
screen sharing; kwm-portal provides only Inhibit.

With Home Manager's NixOS integration and `home-manager.useUserPackages = true`,
include these paths in the NixOS configuration, as required by Home Manager:

```nix
environment.pathsToLink = [ "/share/xdg-desktop-portal" "/share/applications" ];
```

In mzwm, `nix/nixos.nix` imports the NixOS module and enables it by default.
The GTK/wlr backends remain configured alongside it. The idle limitation
described above still applies. Rebuild the system to activate configuration changes.

`nix flake check` evaluates both modules with disabled/enabled configurations,
package overrides, custom desktop names, explicit routing overrides and existing
portal settings, in addition to running the service tests.

## GlobalShortcuts follow-up

The intended path is application -> portal frontend -> kwm-portal -> kwm -> River.
kwm should own dynamic River bindings, report press/release events, and resolve
conflicts with static bindings. kwm-portal should own requests, saved choices and
approval/remapping UI. The private kwm bridge still needs implementation, including
cleanup/re-registration on reconnect and explicit lock/mode behavior.

References:

- [Inhibit backend interface](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.impl.portal.Inhibit.html)
- [logind D-Bus interface](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html)
