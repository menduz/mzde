//! The plan: the mounts, the environment, the network and the command of a
//! sandbox, for a backend. Refer to README.md for the format.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;

use crate::config::{Access, Config, Env, MountMode, Net, Service};
use crate::host::Host;
use crate::util::{Result, expand};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub version: u32,
    pub name: String,
    pub hostname: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub cwd: String,
    pub tty: bool,
    /// "none": the backend makes an empty network namespace; "inherit": the
    /// backend uses the network namespace that it starts in.
    pub network: String,
    /// For `start` and `exec`: the directory of the container.
    pub state: String,
    pub mounts: Vec<Mount>,
    pub env: BTreeMap<String, String>,
    pub command: Vec<String>,
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    #[serde(rename = "type")]
    pub kind: MountType,
    pub source: String,
    pub target: String,
    /// "ro" or "rw" for a bind or a device, the octal permission for a tmpfs.
    pub mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    Bind,
    Dev,
    Tmpfs,
}

/// The use of a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A container for this run only (a run without a scope).
    Run,
    /// The container of the sandbox (the service sandbox@NAME).
    Container,
    /// A run in the container of the sandbox: the backend uses only the
    /// command, the environment, the directory and the terminal.
    Join,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "run" => Some(Mode::Run),
            "container" => Some(Mode::Container),
            "join" => Some(Mode::Join),
            _ => None,
        }
    }
}

/// What the plan needs from the caller.
pub struct Request<'a> {
    pub config: &'a Config,
    pub name: &'a str,
    pub mode: Mode,
    /// The resolv.conf of a private network.
    pub resolv: String,
    /// The Wayland socket of the sandbox: the socket of its security
    /// context (wayland.rs), not the socket of the compositor. None: the
    /// caller could not make it, and the sandbox gets no Wayland socket.
    pub wayland: Option<String>,
    /// The socket of the notifications of the host (notify_host.rs). None:
    /// the notifications of the sandbox give an error.
    pub notify: Option<String>,
    /// The state directory of the container ("" for Mode::Run).
    pub state: String,
    /// The command after sandbox-init.
    pub command: Vec<String>,
}

/// A plan, and the facts about it that the frontend needs.
pub struct Built {
    pub plan: Plan,
    pub net: Net,
    /// The sandbox has the current directory.
    pub has_cwd: bool,
    /// The name of the backend (after SANDBOX_BACKEND) and its program.
    pub backend: (String, String),
}

/// The plan in construction.
struct Builder<'a> {
    host: &'a dyn Host,
    mounts: Vec<Mount>,
    /// The paths that are at the same place in the sandbox and on the host.
    same_paths: Vec<String>,
    env: BTreeMap<String, String>,
}

/// The mode of a bind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bind {
    Ro,
    Rw,
    Dev,
}

impl Builder<'_> {
    fn mnt(&mut self, kind: MountType, source: &str, target: &str, mode: &str) {
        self.mounts.push(Mount {
            kind,
            source: source.into(),
            target: target.into(),
            mode: mode.into(),
        });
    }

    /// Bind `src` on `dst` if `src` exists; else nothing.
    fn bind_to(&mut self, mode: Bind, src: &str, dst: &str) {
        if !self.host.exists(src) {
            return;
        }
        match mode {
            Bind::Dev => self.mnt(MountType::Dev, src, dst, "rw"),
            Bind::Ro => self.mnt(MountType::Bind, src, dst, "ro"),
            Bind::Rw => self.mnt(MountType::Bind, src, dst, "rw"),
        }
        if src == dst {
            self.same_paths.push(dst.into());
        }
    }

    fn bind(&mut self, mode: Bind, src: &str) {
        self.bind_to(mode, src, src);
    }

    fn tmpfs(&mut self, target: &str, mode: &str) {
        self.mnt(MountType::Tmpfs, "", target, mode);
    }

    fn setenv(&mut self, k: &str, v: &str) {
        self.env.insert(k.into(), v.into());
    }
}

/// Make the plan of the sandbox `req.name`.
pub fn build(req: &Request) -> Result<Built> {
    build_on(req, &crate::host::System)
}

/// Make the plan of the sandbox `req.name` for the host `host`.
pub fn build_on(req: &Request, host: &dyn Host) -> Result<Built> {
    let config = req.config;
    let e: &Env = config.env(req.name)?;
    let backend = config.backend_for(e, host.var_nonempty("SANDBOX_BACKEND"))?;
    let uid = host.uid();
    let home = host.home()?;
    let run = host.runtime_dir();
    let net = e.net();
    let mode = req.mode;

    let mut b = Builder {
        host,
        mounts: Vec::new(),
        same_paths: Vec::new(),
        env: BTreeMap::new(),
    };

    // The program has a terminal if this command has one.
    let tty = mode != Mode::Container && host.tty();

    // The environment: only the names of passEnv go in. The steps below can
    // replace a value.
    for v in &e.pass_env {
        if let Some(val) = host.var(v) {
            b.setenv(v, &val);
        }
    }
    for v in ["DISPLAY", "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS", "WAYLAND_DISPLAY"] {
        b.env.remove(v);
    }

    // sandbox-init starts the private services before the command. In the
    // container, it starts them and stays; in a run in the container, it
    // gets their variables (DISPLAY, DBUS_SESSION_BUS_ADDRESS) before the
    // command.
    let mut init = false;
    match mode {
        Mode::Container => {
            b.setenv("SANDBOX_INIT_HOLD", "1");
            init = true;
        }
        Mode::Join => {
            b.setenv("SANDBOX_INIT_JOIN", "1");
            init = true;
        }
        Mode::Run => {}
    }

    // The base system, read-only.
    b.bind(Bind::Ro, "/nix/store");
    b.bind(Bind::Ro, "/etc");
    // ssh_config without the Include of files of /nix/store: ssh refuses
    // them, because their owner is nobody in the sandbox. Refer to
    // module.nix. On NixOS, /etc/ssh/ssh_config is a link, and a backend
    // cannot mount on a link: the mount goes on the file at the end of the
    // link.
    if let Some(ssh) = &config.ssh_config
        && host.exists("/etc/ssh/ssh_config")
    {
        b.bind_to(Bind::Ro, ssh, &host.readlink_f("/etc/ssh/ssh_config"));
    }
    for p in ["/bin", "/usr", "/run/current-system", "/run/opengl-driver", "/run/opengl-driver-32"] {
        b.bind(Bind::Ro, p);
    }

    // The name server. /etc/resolv.conf can be a link into /run.
    let resolv = host.readlink_f("/etc/resolv.conf");
    match net {
        Net::Host => {
            if resolv != "/etc/resolv.conf" {
                b.bind(Bind::Ro, &resolv);
            }
        }
        Net::Private => b.mnt(MountType::Bind, &req.resolv, &resolv, "ro"),
        Net::None => {}
    }

    if e.mount_tmp {
        b.bind(Bind::Rw, "/tmp");
    } else if e.shared_tmp {
        let shared = host.shared_tmp(req.name)?;
        b.mnt(MountType::Bind, &shared, "/tmp", "rw");
    } else {
        b.tmpfs("/tmp", "1777");
    }
    b.tmpfs(&run, "0700");

    // The home directory.
    if e.allow_home {
        b.bind(Bind::Rw, &home);
    } else if e.persistent_home {
        let dir = expand(&e.home_directory, &home);
        host.private_dir(&dir)?;
        b.mnt(MountType::Bind, &dir, &home, "rw");
    } else {
        b.tmpfs(&home, "0700");
    }

    for p in &e.home_patterns {
        let (mode, p) = if let Some(p) = p.strip_prefix("rw:") {
            (Bind::Rw, p)
        } else if let Some(p) = p.strip_prefix("ro:") {
            (Bind::Ro, p)
        } else {
            (Bind::Ro, p.as_str())
        };
        b.bind(mode, &format!("{home}/{p}"));
    }

    for (path, source) in &e.files {
        b.mnt(MountType::Bind, source, &format!("{home}/{path}"), "ro");
    }

    // The capabilities.
    if e.gui {
        // Never the socket of the compositor: with it, a program of the
        // sandbox gets the privileged globals (it can type into the windows
        // of the host). The caller makes the socket of the security context
        // before the plan (wayland.rs).
        match &req.wayland {
            Some(wl) => {
                b.mnt(MountType::Bind, wl, &format!("{run}/wayland-0"), "rw");
                b.setenv("WAYLAND_DISPLAY", "wayland-0");
            }
            None => host.warn("no Wayland socket for the sandbox"),
        }
        b.bind(Bind::Dev, "/dev/dri");
        b.bind(Bind::Ro, "/sys");
        b.setenv("XDG_SESSION_TYPE", "wayland");
    }

    // X11. "private": an X server for the container (xwayland-satellite),
    // so an X11 client sees no X11 client of the host or of other
    // sandboxes. "host": the X server of the host, without isolation.
    if e.x11 == Service::Private && e.gui {
        if mode != Mode::Join {
            b.setenv("SANDBOX_INIT_X11", "1");
            // The abstract X11 sockets are in the network namespace. In the
            // namespace of the host, use a display number that the host and
            // the other sandboxes probably do not use.
            if net == Net::Host {
                b.setenv("SANDBOX_INIT_X11_DISPLAY", &(100 + host.random() % 800).to_string());
            }
            init = true;
        }
    } else if e.x11 == Service::Host {
        b.bind(Bind::Ro, "/tmp/.X11-unix");
        if let Some(d) = host.var_nonempty("DISPLAY") {
            b.setenv("DISPLAY", &d);
        }
        if let Some(x) = host.var_nonempty("XAUTHORITY") {
            b.bind(Bind::Ro, &x);
            b.setenv("XAUTHORITY", &x);
        }
    }

    if e.audio {
        b.bind(Bind::Rw, &format!("{run}/pipewire-0"));
        b.bind(Bind::Rw, &format!("{run}/pulse"));
        b.bind(Bind::Dev, "/dev/snd");
    }

    // D-Bus. "private": a session bus for the container. "host": the session
    // bus of the host, without isolation.
    if e.dbus == Service::Host {
        b.bind(Bind::Rw, &format!("{run}/bus"));
        b.setenv("DBUS_SESSION_BUS_ADDRESS", &format!("unix:path={run}/bus"));
    } else {
        if e.dbus == Service::Private && mode != Mode::Join {
            b.setenv("SANDBOX_INIT_DBUS", "1");
            init = true;
        }
        // The notifications of the private bus go to the host on the socket
        // of the container (notify_bridge.rs, notify_host.rs).
        if let (Service::Private, true, Some(sock)) = (&e.dbus, e.notifications, &req.notify) {
            let dst = format!("{run}/sandbox-notify");
            b.mnt(MountType::Bind, sock, &dst, "rw");
            if mode != Mode::Join {
                b.setenv("SANDBOX_INIT_NOTIFY", &dst);
            }
        }
        // There is no accessibility bus, and GTK must not look for it.
        b.setenv("NO_AT_BRIDGE", "1");
        b.setenv("GTK_A11Y", "none");
    }

    // The configuration of the programs (ssh, git, claude, ...). With
    // xdgRemap, `remap` is the XDG directory of the program, and it
    // replaces the path if it is on the host.
    for m in &e.config_mounts {
        let mut src = host.host_path(&m.path, &home);
        if !m.remap.is_empty() {
            let r = host.host_path(&m.remap, &home);
            if host.exists(&r) {
                src = r;
            }
        }
        // A writable directory that is not on the host: make it, so that the
        // files of the program go to the host and not to the sandbox home.
        if m.mode == MountMode::Rw && m.path.ends_with('/') {
            host.make_dir(&src)?;
        }
        let mode = if m.mode == MountMode::Rw { Bind::Rw } else { Bind::Ro };
        let src = src.strip_suffix('/').unwrap_or(&src).to_string();
        let path = m.path.strip_suffix('/').unwrap_or(&m.path);
        b.bind_to(mode, &src, &format!("{home}/{path}"));
    }

    if e.ssh != Access::None
        && let Some(sock) = host.var_nonempty("SSH_AUTH_SOCK")
        && host.is_socket(&sock)
    {
        b.bind(Bind::Rw, &sock);
        b.setenv("SSH_AUTH_SOCK", &sock);
    }

    if e.gpg {
        b.bind(Bind::Rw, &format!("{home}/.gnupg"));
        b.bind(Bind::Rw, &host.gpg_socketdir(&run));
    }

    if e.docker {
        let from_env = host.var("DOCKER_HOST").unwrap_or_default();
        let from_env = from_env.strip_prefix("unix://").unwrap_or(&from_env).to_string();
        let candidates = [from_env, "/run/docker.sock".into(), "/var/run/docker.sock".into(), format!("{run}/docker.sock")];
        match candidates.iter().find(|s| !s.is_empty() && host.is_socket(s)) {
            Some(sock) => {
                b.bind(Bind::Rw, sock);
                b.setenv("DOCKER_HOST", &format!("unix://{sock}"));
            }
            None => host.warn("docker: no Docker socket found"),
        }
    }

    if e.fuse {
        b.bind(Bind::Dev, "/dev/fuse");
    }
    if e.kvm {
        b.bind(Bind::Dev, "/dev/kvm");
        b.bind(Bind::Dev, "/dev/vfio");
    }
    if e.nvidia {
        for d in &host.devices("nvidia") {
            b.bind(Bind::Dev, d);
        }
        b.bind(Bind::Ro, "/sys");
    }
    if e.libvirt {
        b.bind(Bind::Rw, "/run/libvirt");
        b.bind(Bind::Rw, &format!("{run}/libvirt"));
    }
    if e.nix {
        // The socket of sandbox-nix-daemon.socket: the Nix daemon does not
        // trust the programs of the sandbox. Refer to module.nix.
        b.bind_to(
            Bind::Rw,
            &format!("{run}/sandbox-nix-daemon"),
            "/nix/var/nix/daemon-socket",
        );
        b.setenv("NIX_REMOTE", "daemon");
        // The caches of Nix on the host: the tarball cache, the fetcher
        // cache, the eval cache and the narinfo cache. Without them, Nix
        // downloads again the sources of the flake inputs that are in the
        // store. The sandbox can write the eval cache of the host. Refer to
        // module.nix.
        b.bind(Bind::Rw, &format!("{home}/.cache/nix"));
    }

    if e.mount_home_cache {
        for p in &config.home_cache_dirs {
            b.bind(Bind::Rw, &format!("{home}/{p}"));
        }
    }
    if e.mount_common_home_folders {
        for p in &config.common_home_folders {
            b.bind(Bind::Rw, &format!("{home}/{p}"));
        }
    }

    // The paths of the configuration: "/p", "ro:/p", "/host:/guest",
    // "ro:/host:/guest".
    for p in &e.paths {
        let (mode, p) = if let Some(p) = p.strip_prefix("ro:") {
            (Bind::Ro, p)
        } else if let Some(p) = p.strip_prefix("rw:") {
            (Bind::Rw, p)
        } else {
            (Bind::Rw, p.as_str())
        };
        let (src, dst) = match p.split_once(':') {
            Some((s, d)) => (expand(s, &home), expand(d, &home)),
            None => {
                let s = expand(p, &home);
                (s.clone(), s)
            }
        };
        if host.exists(&src) {
            b.bind_to(mode, &src, &dst);
        } else {
            host.warn(&format!("{src} does not exist; the sandbox does not get it"));
        }
    }

    b.setenv("XDG_RUNTIME_DIR", &run);
    b.setenv("SANDBOX_NAME", req.name);
    b.setenv("SANDBOX_COLOR", &e.color);
    b.setenv("SANDBOX_BACKEND", &backend.0);
    if let Some(t) = e.gtk_theme.as_deref().filter(|t| !t.is_empty()) {
        b.setenv("GTK_THEME", t);
    }
    // The bin directories of the packages come first, the last package
    // first of all.
    let mut path = host.var("PATH").unwrap_or_default();
    for p in &e.bin {
        path = format!("{p}:{path}");
    }
    b.setenv("PATH", &path);
    for (k, v) in &e.env {
        b.setenv(k, v);
    }

    // The program starts in the current directory if the sandbox has it.
    let pwd = host.pwd();
    let has_cwd = b
        .same_paths
        .iter()
        .any(|p| p == "/" || pwd == *p || pwd.starts_with(&format!("{p}/")));
    let cwd = if has_cwd { pwd } else { home.clone() };

    let mut command = Vec::new();
    if init {
        command.push(config.init.clone());
    }
    command.extend(req.command.iter().cloned());

    let plan = Plan {
        version: 1,
        name: req.name.into(),
        hostname: e.hostname.clone(),
        uid,
        gid: host.gid(),
        home,
        cwd,
        tty,
        network: if net == Net::None { "none".into() } else { "inherit".into() },
        state: req.state.clone(),
        mounts: b.mounts,
        env: b.env,
        command,
        extra_args: e.backend_args.get(&backend.0).cloned().unwrap_or_default(),
    };
    Ok(Built {
        plan,
        net,
        has_cwd,
        backend,
    })
}

impl Plan {
    pub fn read(path: &str) -> Result<Plan> {
        let text = fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        serde_json::from_slice(&text).map_err(|e| format!("{path}: {e}"))
    }

    pub fn write(&self, path: &str) -> Result<()> {
        let text = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| format!("cannot write {path}: {e}"))
    }
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
