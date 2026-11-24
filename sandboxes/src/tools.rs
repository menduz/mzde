//! The programs that the command starts. The Nix build gives their absolute
//! paths (environment variables of the build, refer to package.nix), so the
//! PATH of the caller does not select them. A build without them (cargo
//! build, cargo test) uses PATH. systemd-run and systemctl come from the
//! configuration (refer to systemd.rs).

macro_rules! tool {
    ($fn:ident, $var:literal, $name:literal) => {
        pub fn $fn() -> &'static str {
            match option_env!($var) {
                Some(p) => p,
                None => $name,
            }
        }
    };
}

tool!(bwrap, "SANDBOX_TOOL_BWRAP", "bwrap");
tool!(runsc, "SANDBOX_TOOL_RUNSC", "runsc");
tool!(script, "SANDBOX_TOOL_SCRIPT", "script");
tool!(slirp4netns, "SANDBOX_TOOL_SLIRP4NETNS", "slirp4netns");
tool!(tun2socks, "SANDBOX_TOOL_TUN2SOCKS", "tun2socks");
tool!(ip, "SANDBOX_TOOL_IP", "ip");
tool!(iptables, "SANDBOX_TOOL_IPTABLES", "iptables");
tool!(ip6tables, "SANDBOX_TOOL_IP6TABLES", "ip6tables");
tool!(nft, "SANDBOX_TOOL_NFT", "nft");
tool!(gpgconf, "SANDBOX_TOOL_GPGCONF", "gpgconf");
tool!(dbus_daemon, "SANDBOX_TOOL_DBUS_DAEMON", "dbus-daemon");
tool!(dbus_session_conf, "SANDBOX_TOOL_DBUS_SESSION_CONF", "/etc/dbus-1/session.conf");
tool!(xwayland_satellite, "SANDBOX_TOOL_XWAYLAND_SATELLITE", "xwayland-satellite");
tool!(notify_send, "SANDBOX_TOOL_NOTIFY_SEND", "notify-send");
tool!(dconf, "SANDBOX_TOOL_DCONF", "dconf");
