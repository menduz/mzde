//! Tests of the plan: each option of a sandbox, each mode, the order of the
//! mounts and the environment. The host is a fake (host.rs).

use super::*;
use crate::host::fake::Fake;
use crate::testutil::{config, config_top};
use serde_json::json;

fn build_mode(c: &Config, h: &Fake, mode: Mode) -> Result<Built> {
    build_on(
        &Request {
            config: c,
            name: "s",
            mode,
            resolv: "/T/resolv.conf".into(),
            state: "/S".into(),
            command: vec!["cmd".into(), "arg".into()],
        },
        h,
    )
}

fn build(c: &Config, h: &Fake) -> Built {
    build_mode(c, h, Mode::Run).expect("the plan")
}

/// The mounts as "type source target mode" lines.
fn lines(b: &Built) -> Vec<String> {
    b.plan
        .mounts
        .iter()
        .map(|m| format!("{} {} {} {}", format!("{:?}", m.kind).to_lowercase(), m.source, m.target, m.mode).replace("  ", " "))
        .collect()
}

fn has(b: &Built, line: &str) -> bool {
    lines(b).iter().any(|l| l == line)
}

fn target(b: &Built, t: &str) -> Option<Mount> {
    b.plan.mounts.iter().rev().find(|m| m.target == t).cloned()
}

fn env<'a>(b: &'a Built, k: &str) -> Option<&'a str> {
    b.plan.env.get(k).map(String::as_str)
}

// The base system and its order.

#[test]
fn base_mounts_in_order() {
    let mut h = Fake::new();
    h.file("/run/opengl-driver");
    let b = build(&config(json!({})), &h);
    assert_eq!(
        &lines(&b)[..6],
        [
            "bind /nix/store /nix/store ro",
            "bind /etc /etc ro",
            "bind /bin /bin ro",
            "bind /usr /usr ro",
            "bind /run/current-system /run/current-system ro",
            "bind /run/opengl-driver /run/opengl-driver ro",
        ]
    );
}

#[test]
fn a_missing_source_is_not_mounted() {
    let h = Fake::new();
    let b = build(&config(json!({})), &h);
    assert!(target(&b, "/run/opengl-driver").is_none());
    assert!(target(&b, "/run/opengl-driver-32").is_none());
    assert!(h.warnings.borrow().is_empty(), "{:?}", h.warnings.borrow());
}

#[test]
fn ssh_config_goes_on_the_target_of_the_link() {
    let mut h = Fake::new();
    h.link("/etc/ssh/ssh_config", "/etc/static/ssh/ssh_config").file("/nix/store/x-ssh_config");
    let c = config_top(json!({}), json!({"sshConfig": "/nix/store/x-ssh_config"}));
    let b = build(&c, &h);
    assert!(has(&b, "bind /nix/store/x-ssh_config /etc/static/ssh/ssh_config ro"), "{:?}", lines(&b));
    // After /etc: it covers the file of /etc.
    let pos = |t: &str| b.plan.mounts.iter().position(|m| m.target == t).unwrap();
    assert!(pos("/etc/static/ssh/ssh_config") > pos("/etc"));
}

#[test]
fn no_ssh_config_without_the_file_of_the_host() {
    let mut h = Fake::new();
    h.file("/nix/store/x-ssh_config");
    let c = config_top(json!({}), json!({"sshConfig": "/nix/store/x-ssh_config"}));
    assert!(!lines(&build(&c, &h)).iter().any(|l| l.contains("x-ssh_config")));
}

// The name server and the network.

#[test]
fn private_network_gets_the_resolv_conf_of_the_run() {
    let mut h = Fake::new();
    h.link("/etc/resolv.conf", "/run/systemd/resolve/stub-resolv.conf");
    let b = build(&config(json!({})), &h);
    assert!(has(&b, "bind /T/resolv.conf /run/systemd/resolve/stub-resolv.conf ro"), "{:?}", lines(&b));
    assert_eq!(b.net, Net::Private);
    assert_eq!(b.plan.network, "inherit");
}

#[test]
fn host_network_gets_the_target_of_resolv_conf() {
    let mut h = Fake::new();
    h.link("/etc/resolv.conf", "/run/systemd/resolve/stub-resolv.conf");
    let b = build(&config(json!({"shareHostNetwork": true})), &h);
    assert!(has(&b, "bind /run/systemd/resolve/stub-resolv.conf /run/systemd/resolve/stub-resolv.conf ro"));
    assert_eq!(b.net, Net::Host);
    // Without a link, /etc has the file.
    let h = Fake::new();
    let b = build(&config(json!({"shareHostNetwork": true})), &h);
    assert!(!lines(&b).iter().any(|l| l.contains("resolv")));
}

#[test]
fn no_network_has_no_resolv_conf() {
    let h = Fake::new();
    let b = build(&config(json!({"disableNetworking": true})), &h);
    assert!(!lines(&b).iter().any(|l| l.contains("resolv")));
    assert_eq!(b.net, Net::None);
    assert_eq!(b.plan.network, "none");
}

#[test]
fn disable_networking_wins_over_host_network() {
    let h = Fake::new();
    let b = build(&config(json!({"disableNetworking": true, "shareHostNetwork": true})), &h);
    assert_eq!(b.net, Net::None);
}

// /tmp and the runtime directory.

#[test]
fn tmp_is_a_tmpfs_by_default() {
    let h = Fake::new();
    let b = build(&config(json!({})), &h);
    assert!(has(&b, "tmpfs /tmp 1777"));
    assert!(has(&b, "tmpfs /run/user/1000 0700"));
}

#[test]
fn mount_tmp_and_shared_tmp() {
    let h = Fake::new();
    assert!(has(&build(&config(json!({"mountTmp": true})), &h), "bind /tmp /tmp rw"));
    let b = build(&config(json!({"sharedTmp": true})), &h);
    assert!(has(&b, "bind /tmp/sandbox-1000-s /tmp rw"));
    assert!(!has(&b, "tmpfs /tmp 1777"));
    // mountTmp replaces sharedTmp.
    let b = build(&config(json!({"mountTmp": true, "sharedTmp": true})), &h);
    assert!(has(&b, "bind /tmp /tmp rw"));
    assert!(!lines(&b).iter().any(|l| l.contains("sandbox-1000-s")));
}

#[test]
fn shared_tmp_error_stops_the_plan() {
    let mut h = Fake::new();
    h.shared_tmp_error = Some("/tmp/sandbox-1000-s is not a directory of this user".into());
    let err = build_mode(&config(json!({"sharedTmp": true})), &h, Mode::Run).err().unwrap();
    assert!(err.contains("is not a directory of this user"), "{err}");
}

// The home directory.

#[test]
fn persistent_home_is_made_and_mounted() {
    let h = Fake::new();
    let b = build(&config(json!({})), &h);
    assert!(has(&b, "bind /home/a/.local/share/sandboxes/s/home /home/a rw"), "{:?}", lines(&b));
    assert_eq!(*h.made.borrow(), ["private /home/a/.local/share/sandboxes/s/home"]);
}

#[test]
fn disposable_home_is_a_tmpfs() {
    let h = Fake::new();
    let b = build(&config(json!({"persistentHome": false})), &h);
    assert!(has(&b, "tmpfs /home/a 0700"));
    assert!(h.made.borrow().is_empty());
}

#[test]
fn allow_home_mounts_the_real_home() {
    let h = Fake::new();
    let b = build(&config(json!({"allowHome": true, "persistentHome": true})), &h);
    assert!(has(&b, "bind /home/a /home/a rw"));
    assert!(h.made.borrow().is_empty());
    // The home directory is at the same place: the program starts in it.
    assert!(b.has_cwd);
}

#[test]
fn home_patterns_with_modes() {
    let mut h = Fake::new();
    h.file("/home/a/.zshrc").file("/home/a/.histfile");
    let b = build(&config(json!({"homePatterns": [".zshrc", "rw:.histfile", "ro:.missing"]})), &h);
    assert!(has(&b, "bind /home/a/.zshrc /home/a/.zshrc ro"));
    assert!(has(&b, "bind /home/a/.histfile /home/a/.histfile rw"));
    assert!(!lines(&b).iter().any(|l| l.contains(".missing")));
}

#[test]
fn files_are_read_only_after_the_home() {
    let h = Fake::new();
    let b = build(&config(json!({"files": {".config/a": "/nix/store/a", ".b": "/nix/store/b"}})), &h);
    let l = lines(&b);
    let home = l.iter().position(|x| x.ends_with(" /home/a rw")).unwrap();
    let a = l.iter().position(|x| x == "bind /nix/store/a /home/a/.config/a ro").unwrap();
    let bb = l.iter().position(|x| x == "bind /nix/store/b /home/a/.b ro").unwrap();
    assert!(home < a && home < bb);
    // In the order of the names (config.json sorts them).
    assert!(bb < a);
}

// gui, X11 and D-Bus.

#[test]
fn gui_mounts_the_wayland_socket_as_wayland_0() {
    let mut h = Fake::new();
    h.set("WAYLAND_DISPLAY", "wayland-1").socket("/run/user/1000/wayland-1").file("/dev/dri").file("/sys");
    let b = build(&config(json!({"gui": true, "x11": "none"})), &h);
    assert!(has(&b, "bind /run/user/1000/wayland-1 /run/user/1000/wayland-0 rw"));
    assert!(has(&b, "dev /dev/dri /dev/dri rw"));
    assert!(has(&b, "bind /sys /sys ro"));
    assert_eq!(env(&b, "WAYLAND_DISPLAY"), Some("wayland-0"));
    assert_eq!(env(&b, "XDG_SESSION_TYPE"), Some("wayland"));
}

#[test]
fn gui_with_an_absolute_or_default_socket() {
    let mut h = Fake::new();
    h.set("WAYLAND_DISPLAY", "/x/wl").socket("/x/wl");
    let b = build(&config(json!({"gui": true, "x11": "none"})), &h);
    assert!(has(&b, "bind /x/wl /run/user/1000/wayland-0 rw"));
    let mut h = Fake::new();
    h.socket("/run/user/1000/wayland-0");
    let b = build(&config(json!({"gui": true, "x11": "none"})), &h);
    assert!(has(&b, "bind /run/user/1000/wayland-0 /run/user/1000/wayland-0 rw"));
}

#[test]
fn gui_without_a_socket_warns() {
    let mut h = Fake::new();
    h.set("WAYLAND_DISPLAY", "wayland-1").file("/run/user/1000/wayland-1");
    let b = build(&config(json!({"gui": true, "x11": "none"})), &h);
    assert!(!lines(&b).iter().any(|l| l.contains("wayland")));
    assert_eq!(env(&b, "WAYLAND_DISPLAY"), None);
    assert_eq!(*h.warnings.borrow(), ["no Wayland socket at /run/user/1000/wayland-1"]);
}

#[test]
fn the_display_variables_of_the_caller_do_not_go_in() {
    let mut h = Fake::new();
    h.set("DISPLAY", ":0").set("WAYLAND_DISPLAY", "wayland-1").set("LANG", "C.UTF-8").set("SECRET", "x");
    let b = build(&config(json!({"x11": "none", "dbus": "none"})), &h);
    assert_eq!(env(&b, "DISPLAY"), None);
    assert_eq!(env(&b, "WAYLAND_DISPLAY"), None);
    // passEnv goes in; other variables do not.
    assert_eq!(env(&b, "LANG"), Some("C.UTF-8"));
    assert_eq!(env(&b, "SECRET"), None);
    assert_eq!(env(&b, "TERM"), None);
}

#[test]
fn private_x11_needs_gui() {
    let h = Fake::new();
    let b = build(&config(json!({"gui": false, "x11": "private", "dbus": "none"})), &h);
    assert_eq!(env(&b, "SANDBOX_INIT_X11"), None);
    assert_eq!(b.plan.command, ["cmd", "arg"]);
}

#[test]
fn private_x11_and_dbus_start_init_in_a_run() {
    let h = Fake::new();
    let b = build(&config(json!({"gui": true})), &h);
    assert_eq!(env(&b, "SANDBOX_INIT_X11"), Some("1"));
    assert_eq!(env(&b, "SANDBOX_INIT_DBUS"), Some("1"));
    // A private network: the display numbers are free.
    assert_eq!(env(&b, "SANDBOX_INIT_X11_DISPLAY"), None);
    assert_eq!(b.plan.command, ["/i/sandbox-init", "cmd", "arg"]);
    assert_eq!(env(&b, "NO_AT_BRIDGE"), Some("1"));
    assert_eq!(env(&b, "GTK_A11Y"), Some("none"));
}

#[test]
fn private_x11_on_the_host_network_gets_a_display_number() {
    let mut h = Fake::new();
    h.rand = 1234;
    let b = build(&config(json!({"gui": true, "shareHostNetwork": true})), &h);
    assert_eq!(env(&b, "SANDBOX_INIT_X11_DISPLAY"), Some("534"));
    h.rand = 799;
    let b = build(&config(json!({"gui": true, "shareHostNetwork": true})), &h);
    assert_eq!(env(&b, "SANDBOX_INIT_X11_DISPLAY"), Some("899"));
}

#[test]
fn host_x11_mounts_the_sockets_of_the_host() {
    let mut h = Fake::new();
    h.file("/tmp/.X11-unix").set("DISPLAY", ":1").set("XAUTHORITY", "/home/a/.Xauthority").file("/home/a/.Xauthority");
    let b = build(&config(json!({"x11": "host", "dbus": "none"})), &h);
    assert!(has(&b, "bind /tmp/.X11-unix /tmp/.X11-unix ro"));
    assert!(has(&b, "bind /home/a/.Xauthority /home/a/.Xauthority ro"));
    assert_eq!(env(&b, "DISPLAY"), Some(":1"));
    assert_eq!(env(&b, "XAUTHORITY"), Some("/home/a/.Xauthority"));
    assert_eq!(b.plan.command, ["cmd", "arg"]);
}

#[test]
fn host_dbus_mounts_the_bus_of_the_host() {
    let mut h = Fake::new();
    h.socket("/run/user/1000/bus");
    let b = build(&config(json!({"dbus": "host", "x11": "none"})), &h);
    assert!(has(&b, "bind /run/user/1000/bus /run/user/1000/bus rw"));
    assert_eq!(env(&b, "DBUS_SESSION_BUS_ADDRESS"), Some("unix:path=/run/user/1000/bus"));
    assert_eq!(env(&b, "NO_AT_BRIDGE"), None);
    assert_eq!(env(&b, "SANDBOX_INIT_DBUS"), None);
}

#[test]
fn no_dbus() {
    let h = Fake::new();
    let b = build(&config(json!({"dbus": "none", "x11": "none"})), &h);
    assert_eq!(env(&b, "SANDBOX_INIT_DBUS"), None);
    assert_eq!(env(&b, "NO_AT_BRIDGE"), Some("1"));
    assert_eq!(b.plan.command, ["cmd", "arg"]);
}

// The modes of the plan.

#[test]
fn container_mode_holds_the_services() {
    let mut h = Fake::new();
    h.terminal = true;
    let b = build_mode(&config(json!({"gui": true})), &h, Mode::Container).unwrap();
    assert_eq!(env(&b, "SANDBOX_INIT_HOLD"), Some("1"));
    assert_eq!(env(&b, "SANDBOX_INIT_X11"), Some("1"));
    assert_eq!(env(&b, "SANDBOX_INIT_DBUS"), Some("1"));
    assert_eq!(b.plan.command, ["/i/sandbox-init", "cmd", "arg"]);
    // The container never has the terminal of the caller.
    assert!(!b.plan.tty);
    assert_eq!(b.plan.state, "/S");
}

#[test]
fn join_mode_gets_the_services_of_the_container() {
    let mut h = Fake::new();
    h.terminal = true;
    let b = build_mode(&config(json!({"gui": true})), &h, Mode::Join).unwrap();
    assert_eq!(env(&b, "SANDBOX_INIT_JOIN"), Some("1"));
    assert_eq!(env(&b, "SANDBOX_INIT_X11"), None);
    assert_eq!(env(&b, "SANDBOX_INIT_DBUS"), None);
    assert_eq!(env(&b, "SANDBOX_INIT_HOLD"), None);
    assert_eq!(b.plan.command, ["/i/sandbox-init", "cmd", "arg"]);
    assert!(b.plan.tty);
}

#[test]
fn join_mode_without_services_still_starts_init() {
    let h = Fake::new();
    let b = build_mode(&config(json!({"x11": "none", "dbus": "none"})), &h, Mode::Join).unwrap();
    assert_eq!(b.plan.command, ["/i/sandbox-init", "cmd", "arg"]);
}

#[test]
fn the_same_mounts_in_all_modes() {
    let mut h = Fake::new();
    h.file("/home/a/code");
    let c = config(json!({"gui": true, "paths": ["~/code"]}));
    let run = build_mode(&c, &h, Mode::Run).unwrap();
    let container = build_mode(&c, &h, Mode::Container).unwrap();
    let join = build_mode(&c, &h, Mode::Join).unwrap();
    assert_eq!(run.plan.mounts, container.plan.mounts);
    assert_eq!(run.plan.mounts, join.plan.mounts);
}

#[test]
fn terminal_of_a_run() {
    let mut h = Fake::new();
    assert!(!build(&config(json!({})), &h).plan.tty);
    h.terminal = true;
    assert!(build(&config(json!({})), &h).plan.tty);
}

// The configuration of the programs.

#[test]
fn config_mounts_read_and_write() {
    let mut h = Fake::new();
    h.file("/home/a/.gitconfig").file("/home/a/.claude").file("/home/a/.config/zed");
    let c = config(json!({"configMounts": [
        {"mode": "ro", "path": ".gitconfig", "remap": ""},
        {"mode": "rw", "path": ".claude/", "remap": ""},
        {"mode": "ro", "path": ".config/zed/", "remap": ""},
    ]}));
    let b = build(&c, &h);
    assert!(has(&b, "bind /home/a/.gitconfig /home/a/.gitconfig ro"));
    assert!(has(&b, "bind /home/a/.claude /home/a/.claude rw"));
    assert!(has(&b, "bind /home/a/.config/zed /home/a/.config/zed ro"));
    // A writable directory is made on the host (the files go to the host);
    // a read-only one is not.
    assert!(h.made.borrow().contains(&"dir /home/a/.claude/".to_string()));
    assert!(!h.made.borrow().iter().any(|m| m.contains("zed")));
}

#[test]
fn config_mounts_come_from_the_xdg_directories() {
    let mut h = Fake::new();
    h.set("XDG_CONFIG_HOME", "/x/conf").file("/x/conf/zed");
    let c = config(json!({"configMounts": [{"mode": "ro", "path": ".config/zed/", "remap": ""}]}));
    assert!(has(&build(&c, &h), "bind /x/conf/zed /home/a/.config/zed ro"));
}

#[test]
fn xdg_remap_if_it_is_on_the_host() {
    let mut h = Fake::new();
    h.file("/home/a/.claude").file("/home/a/.config/claude");
    let c = config(json!({"configMounts": [{"mode": "rw", "path": ".claude/", "remap": ".config/claude/"}]}));
    assert!(has(&build(&c, &h), "bind /home/a/.config/claude /home/a/.claude rw"));
    // Not on the host: the path itself.
    let mut h = Fake::new();
    h.file("/home/a/.claude");
    assert!(has(&build(&c, &h), "bind /home/a/.claude /home/a/.claude rw"));
}

#[test]
fn config_mounts_cover_home_patterns_and_files() {
    let mut h = Fake::new();
    h.file("/home/a/.gitconfig");
    let c = config(json!({
        "files": {".gitconfig": "/nix/store/g"},
        "configMounts": [{"mode": "ro", "path": ".gitconfig", "remap": ""}],
    }));
    let b = build(&c, &h);
    // The last mount on the target is the one of the access option.
    assert_eq!(target(&b, "/home/a/.gitconfig").unwrap().source, "/home/a/.gitconfig");
}

#[test]
fn ssh_agent_only_with_ssh() {
    let mut h = Fake::new();
    h.set("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent").socket("/run/user/1000/ssh-agent");
    let b = build(&config(json!({})), &h);
    assert_eq!(env(&b, "SSH_AUTH_SOCK"), None);
    assert!(!lines(&b).iter().any(|l| l.contains("ssh-agent")));
    let b = build(&config(json!({"ssh": "read"})), &h);
    assert!(has(&b, "bind /run/user/1000/ssh-agent /run/user/1000/ssh-agent rw"));
    assert_eq!(env(&b, "SSH_AUTH_SOCK"), Some("/run/user/1000/ssh-agent"));
    // A path that is not a socket is not the agent.
    let mut h = Fake::new();
    h.set("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent").file("/run/user/1000/ssh-agent");
    assert_eq!(env(&build(&config(json!({"ssh": "read"})), &h), "SSH_AUTH_SOCK"), None);
}

#[test]
fn gpg_mounts_the_home_and_the_sockets() {
    let mut h = Fake::new();
    h.file("/home/a/.gnupg").file("/run/user/1000/gnupg/d.x");
    h.gpg = Some("/run/user/1000/gnupg/d.x".into());
    let b = build(&config(json!({"gpg": true})), &h);
    assert!(has(&b, "bind /home/a/.gnupg /home/a/.gnupg rw"));
    assert!(has(&b, "bind /run/user/1000/gnupg/d.x /run/user/1000/gnupg/d.x rw"));
}

#[test]
fn docker_socket_choice() {
    let mut h = Fake::new();
    h.set("DOCKER_HOST", "unix:///run/user/1000/podman.sock").socket("/run/user/1000/podman.sock").socket("/run/docker.sock");
    let b = build(&config(json!({"docker": true})), &h);
    assert_eq!(env(&b, "DOCKER_HOST"), Some("unix:///run/user/1000/podman.sock"));
    // Without DOCKER_HOST: /run/docker.sock first.
    h.vars.remove("DOCKER_HOST");
    let b = build(&config(json!({"docker": true})), &h);
    assert!(has(&b, "bind /run/docker.sock /run/docker.sock rw"));
    assert_eq!(env(&b, "DOCKER_HOST"), Some("unix:///run/docker.sock"));
    // The runtime directory last.
    let mut h = Fake::new();
    h.socket("/run/user/1000/docker.sock");
    assert_eq!(env(&build(&config(json!({"docker": true})), &h), "DOCKER_HOST"), Some("unix:///run/user/1000/docker.sock"));
}

#[test]
fn docker_without_a_socket_warns() {
    let h = Fake::new();
    let b = build(&config(json!({"docker": true})), &h);
    assert_eq!(env(&b, "DOCKER_HOST"), None);
    assert_eq!(*h.warnings.borrow(), ["docker: no Docker socket found"]);
}

#[test]
fn devices() {
    let mut h = Fake::new();
    h.file("/dev/fuse").file("/dev/kvm").file("/dev/snd").file("/sys");
    h.file("/dev/nvidiactl").file("/dev/nvidia0").file("/dev/nvidia-uvm");
    h.devs = vec!["/dev/nvidiactl".into(), "/dev/nvidia0".into(), "/dev/nvidia-uvm".into()];
    let b = build(&config(json!({"fuse": true, "kvm": true, "nvidia": true})), &h);
    assert!(has(&b, "dev /dev/fuse /dev/fuse rw"));
    assert!(has(&b, "dev /dev/kvm /dev/kvm rw"));
    // /dev/vfio is not on this host.
    assert!(!lines(&b).iter().any(|l| l.contains("vfio")));
    let nv: Vec<String> = lines(&b).into_iter().filter(|l| l.contains("nvidia")).collect();
    assert_eq!(nv, ["dev /dev/nvidia-uvm /dev/nvidia-uvm rw", "dev /dev/nvidia0 /dev/nvidia0 rw", "dev /dev/nvidiactl /dev/nvidiactl rw"]);
    assert!(has(&b, "bind /sys /sys ro"));
}

#[test]
fn audio_and_libvirt() {
    let mut h = Fake::new();
    h.socket("/run/user/1000/pipewire-0").file("/run/user/1000/pulse").file("/dev/snd").file("/run/libvirt");
    let b = build(&config(json!({"audio": true, "libvirt": true})), &h);
    assert!(has(&b, "bind /run/user/1000/pipewire-0 /run/user/1000/pipewire-0 rw"));
    assert!(has(&b, "bind /run/user/1000/pulse /run/user/1000/pulse rw"));
    assert!(has(&b, "dev /dev/snd /dev/snd rw"));
    assert!(has(&b, "bind /run/libvirt /run/libvirt rw"));
}

#[test]
fn nix_gets_the_untrusted_daemon_socket() {
    let mut h = Fake::new();
    h.file("/run/user/1000/sandbox-nix-daemon");
    let b = build(&config(json!({"nix": true})), &h);
    assert!(has(&b, "bind /run/user/1000/sandbox-nix-daemon /nix/var/nix/daemon-socket rw"), "{:?}", lines(&b));
    assert_eq!(env(&b, "NIX_REMOTE"), Some("daemon"));
    // Never the socket of the daemon of the host.
    assert!(!lines(&b).iter().any(|l| l.starts_with("bind /nix/var/nix/daemon-socket")));
}

#[test]
fn caches_and_common_folders() {
    let mut h = Fake::new();
    h.file("/home/a/.cache/pip").file("/home/a/.cargo");
    let b = build(&config(json!({"mountHomeCache": true, "mountCommonHomeFolders": true})), &h);
    assert!(has(&b, "bind /home/a/.cache/pip /home/a/.cache/pip rw"));
    assert!(has(&b, "bind /home/a/.cargo /home/a/.cargo rw"));
}

// The paths of the configuration and the current directory.

#[test]
fn paths_with_modes_and_targets() {
    let mut h = Fake::new();
    h.file("/home/a/code").file("/data").file("/srv/x");
    let c = config(json!({"paths": ["~/code/", "ro:/data", "rw:/srv/x:~/x", "/missing"]}));
    let b = build(&c, &h);
    assert!(has(&b, "bind /home/a/code /home/a/code rw"));
    assert!(has(&b, "bind /data /data ro"));
    assert!(has(&b, "bind /srv/x /home/a/x rw"));
    assert_eq!(*h.warnings.borrow(), ["/missing does not exist; the sandbox does not get it"]);
}

#[test]
fn the_program_starts_in_the_current_directory_of_a_path() {
    let mut h = Fake::new();
    h.file("/home/a/code");
    let c = config(json!({"paths": ["~/code"]}));
    h.cwd = "/home/a/code/app".into();
    let b = build(&c, &h);
    assert!(b.has_cwd);
    assert_eq!(b.plan.cwd, "/home/a/code/app");
    h.cwd = "/home/a/code".into();
    assert_eq!(build(&c, &h).plan.cwd, "/home/a/code");
}

#[test]
fn a_prefix_is_not_a_parent_directory() {
    let mut h = Fake::new();
    h.file("/home/a/code");
    h.cwd = "/home/a/codex".into();
    let b = build(&config(json!({"paths": ["~/code"]})), &h);
    assert!(!b.has_cwd);
    assert_eq!(b.plan.cwd, "/home/a");
}

#[test]
fn a_path_at_a_different_place_does_not_give_the_directory() {
    let mut h = Fake::new();
    h.file("/srv/x");
    h.cwd = "/srv/x".into();
    let b = build(&config(json!({"paths": ["/srv/x:/home/a/x"]})), &h);
    assert!(!b.has_cwd);
    assert_eq!(b.plan.cwd, "/home/a");
}

#[test]
fn a_root_path_gives_every_directory() {
    let mut h = Fake::new();
    h.file("/");
    h.cwd = "/var/lib".into();
    let b = build(&config(json!({"paths": ["/"]})), &h);
    assert!(b.has_cwd);
    assert_eq!(b.plan.cwd, "/var/lib");
}

#[test]
fn the_base_system_gives_its_directories() {
    // /nix/store is at the same place, so a program in /nix/store starts
    // there (as in sandbox.sh).
    let mut h = Fake::new();
    h.cwd = "/nix/store/abc".into();
    assert_eq!(build(&config(json!({})), &h).plan.cwd, "/nix/store/abc");
}

// The environment.

#[test]
fn labels_and_theme() {
    let h = Fake::new();
    let b = build(&config(json!({"gtkTheme": "Win:dark", "color": "#123456"})), &h);
    assert_eq!(env(&b, "SANDBOX_NAME"), Some("s"));
    assert_eq!(env(&b, "SANDBOX_COLOR"), Some("#123456"));
    assert_eq!(env(&b, "SANDBOX_BACKEND"), Some("bwrap"));
    assert_eq!(env(&b, "GTK_THEME"), Some("Win:dark"));
    assert_eq!(env(&b, "XDG_RUNTIME_DIR"), Some("/run/user/1000"));
    let b = build(&config(json!({"gtkTheme": ""})), &h);
    assert_eq!(env(&b, "GTK_THEME"), None);
}

#[test]
fn path_has_the_bins_first_the_last_package_first_of_all() {
    let h = Fake::new();
    let b = build(&config(json!({"bin": ["/nix/store/a/bin", "/nix/store/b/bin"]})), &h);
    assert_eq!(env(&b, "PATH"), Some("/nix/store/b/bin:/nix/store/a/bin:/usr/bin"));
}

#[test]
fn env_of_the_sandbox_comes_last() {
    let mut h = Fake::new();
    h.set("LANG", "C");
    let b = build(&config(json!({"env": {"LANG": "fi_FI.UTF-8", "SANDBOX_NAME": "other", "FOO": "a=b"}})), &h);
    assert_eq!(env(&b, "LANG"), Some("fi_FI.UTF-8"));
    assert_eq!(env(&b, "SANDBOX_NAME"), Some("other"));
    assert_eq!(env(&b, "FOO"), Some("a=b"));
}

#[test]
fn an_empty_variable_of_pass_env_goes_in() {
    let mut h = Fake::new();
    h.set("LANG", "");
    assert_eq!(env(&build(&config(json!({})), &h), "LANG"), Some(""));
}

// The fields of the plan and the backend.

#[test]
fn the_fields_of_the_plan() {
    let h = Fake::new();
    let b = build(&config(json!({"hostname": "box", "backendArgs": {"bwrap": ["--x"], "gvisor": ["--y"]}})), &h);
    let p = &b.plan;
    assert_eq!((p.version, p.name.as_str(), p.hostname.as_str()), (1, "s", "box"));
    assert_eq!((p.uid, p.gid, p.home.as_str()), (1000, 100, "/home/a"));
    assert_eq!(p.extra_args, ["--x"]);
    assert_eq!(b.backend, ("bwrap".to_string(), "/b/bwrap".to_string()));
}

#[test]
fn sandbox_backend_replaces_the_backend() {
    let mut h = Fake::new();
    h.set("SANDBOX_BACKEND", "gvisor");
    let b = build(&config(json!({"backendArgs": {"gvisor": ["--y"]}})), &h);
    assert_eq!(b.backend, ("gvisor".to_string(), "/b/gvisor".to_string()));
    assert_eq!(b.plan.extra_args, ["--y"]);
    assert_eq!(env(&b, "SANDBOX_BACKEND"), Some("gvisor"));
    // An empty value is no value.
    h.set("SANDBOX_BACKEND", "");
    assert_eq!(build(&config(json!({})), &h).backend.0, "bwrap");
}

#[test]
fn runsc_is_gvisor() {
    let h = Fake::new();
    assert_eq!(build(&config(json!({"backend": "runsc"})), &h).backend.0, "gvisor");
}

#[test]
fn unknown_backend_and_sandbox() {
    let h = Fake::new();
    let err = build_mode(&config(json!({"backend": "nope"})), &h, Mode::Run).err().unwrap();
    assert_eq!(err, "no backend has the name 'nope'. Run 'sandbox backends'.");
    let c = config(json!({}));
    let err = build_on(
        &Request {
            config: &c,
            name: "other",
            mode: Mode::Run,
            resolv: String::new(),
            state: String::new(),
            command: vec![],
        },
        &h,
    )
    .err()
    .unwrap();
    assert_eq!(err, "no sandbox has the name 'other'. Run 'sandbox list'.");
}

#[test]
fn no_home_is_an_error() {
    let mut h = Fake::new();
    h.vars.remove("HOME");
    assert_eq!(build_mode(&config(json!({})), &h, Mode::Run).err().unwrap(), "HOME is not set");
}

#[test]
fn the_plan_as_json() {
    let h = Fake::new();
    let b = build(&config(json!({"x11": "none", "dbus": "none"})), &h);
    let v = serde_json::to_value(&b.plan).unwrap();
    // The names of the fields of README.md.
    for k in ["version", "name", "hostname", "uid", "gid", "home", "cwd", "tty", "network", "state", "mounts", "env", "command", "extraArgs"] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert_eq!(v["mounts"][0], json!({"type": "bind", "source": "/nix/store", "target": "/nix/store", "mode": "ro"}));
    // A plan reads back as the same plan.
    let back: Plan = serde_json::from_value(v).unwrap();
    assert_eq!(back, b.plan);
}
