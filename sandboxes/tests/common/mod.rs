//! Helpers of the integration tests: a test directory with a configuration,
//! a home directory, a runtime directory and the links of the program.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

pub const BIN: &str = env!("CARGO_BIN_EXE_sandbox");

/// A directory for one test.
pub struct Dir {
    pub root: PathBuf,
}

impl Dir {
    pub fn new(tag: &str) -> Dir {
        let root = std::env::temp_dir().join(format!("sandbox-it-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for d in ["home", "run", "bin"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("run"), fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["sandbox-init", "sandbox-backend-bwrap", "sandbox-backend-gvisor"] {
            std::os::unix::fs::symlink(BIN, root.join("bin").join(name)).unwrap();
        }
        Dir { root }
    }

    pub fn path(&self, p: &str) -> String {
        self.root.join(p).to_str().unwrap().to_string()
    }

    /// Write config.json with the sandboxes `envs` (name → changes of the
    /// defaults) and `top` (changes of the top level).
    pub fn config(&self, envs: Value, top: Value) -> String {
        let mut environments = serde_json::Map::new();
        for (name, patch) in envs.as_object().unwrap() {
            let mut e = env_json(name, &self.path(&format!("state/{name}/home")));
            merge(&mut e, patch);
            environments.insert(name.clone(), e);
        }
        let mut c = json!({
            "menu": {"command": "head -n 1", "markup": false},
            "host": {"color": "#404040"},
            "backends": {"bwrap": self.path("bin/sandbox-backend-bwrap"), "gvisor": self.path("bin/sandbox-backend-gvisor")},
            "init": self.path("bin/sandbox-init"),
            "sshConfig": null,
            "homeCacheDirs": [],
            "commonHomeFolders": [],
            "opensnitch": null,
            "environments": environments,
        });
        merge(&mut c, &top);
        let p = self.path("config.json");
        fs::write(&p, serde_json::to_vec_pretty(&c).unwrap()).unwrap();
        p
    }

    /// The command `sandbox ARGS` with the configuration, the home and the
    /// runtime directory of this test, and no scope.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("SANDBOX_CONFIG", self.path("config.json"))
            .env("HOME", self.path("home"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("SANDBOX_NO_SCOPE", "1")
            .env_remove("SANDBOX__SCOPED")
            .env_remove("SANDBOX_NAME")
            .env_remove("SANDBOX_BACKEND")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("SSH_AUTH_SOCK")
            .current_dir(self.path("home"))
            .stdin(Stdio::null());
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A sandbox with the defaults of module.nix, without gui, X11 and D-Bus.
pub fn env_json(name: &str, home: &str) -> Value {
    json!({
        "description": format!("sandbox {name}"), "color": "#008080", "gtkTheme": null, "backend": "bwrap", "hostname": name,
        "persistentHome": true, "homeDirectory": home, "homePatterns": [], "paths": [], "env": {},
        "sshWrite": false, "gpg": false, "docker": false, "fuse": false, "gui": false,
        "x11": "none", "dbus": "none", "nvidia": false, "kvm": false, "audio": false,
        "libvirt": false, "nix": false, "allowHome": false, "internetAccess": true, "lanAccess": true,
        "allowHosts": [], "disableNetworking": true, "shareHostNetwork": false,
        "mountHomeCache": false, "mountCommonHomeFolders": false, "mountTmp": false, "sharedTmp": false,
        "cleanTmp": true, "privacyFilter": false, "yolo": false, "backendArgs": {},
        "ssh": "none", "git": "none", "claude": "none", "codex": "none", "gemini": "none",
        "hermes": "none", "opencode": "none", "zed": "none", "direnv": "none",
        "configMounts": [], "socksProxy": null, "allowHostLoopback": false,
        "passEnv": ["HOME", "PATH"],
        "opensnitch": {"enable": false, "directory": format!("{home}/../opensnitch"), "rules": null},
        "files": {}, "bin": []
    })
}

pub fn merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(bv) if bv.is_object() && v.is_object() => merge(bv, v),
                    _ => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// Whether bwrap can make a sandbox here (user namespaces). A Nix build
/// can run without them.
pub fn bwrap_works() -> bool {
    let bwrap = option_env!("SANDBOX_TOOL_BWRAP").unwrap_or("bwrap");
    let ok = Command::new(bwrap)
        .args(["--unshare-user", "--unshare-pid", "--unshare-net", "--dev", "/dev", "--proc", "/proc", "--ro-bind", "/", "/", "true"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("bwrap cannot make a sandbox here: the test does not run");
    }
    ok
}
