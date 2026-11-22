//! Helpers for the tests: a configuration with one sandbox "s".

use crate::config::Config;
use serde_json::{Value, json};

/// A sandbox "s" with the defaults of module.nix, except gui (false here:
/// the tests of gui set it).
pub fn env_json() -> Value {
    json!({
        "description": "", "color": "#008080", "gtkTheme": null, "backend": "bwrap", "hostname": "s",
        "persistentHome": true, "homeDirectory": "~/.local/share/sandboxes/s/home",
        "homePatterns": [], "paths": [], "env": {},
        "sshWrite": false, "gpg": false, "docker": false, "fuse": false, "gui": false,
        "x11": "private", "dbus": "private", "notifications": true, "nvidia": false, "kvm": false, "audio": false,
        "libvirt": false, "nix": false, "allowHome": false, "internetAccess": true, "lanAccess": true,
        "allowHosts": [], "disableNetworking": false, "shareHostNetwork": false,
        "mountHomeCache": false, "mountCommonHomeFolders": false, "mountTmp": false, "sharedTmp": false,
        "cleanTmp": true, "privacyFilter": false, "yolo": false, "backendArgs": {},
        "ssh": "none", "git": "none", "claude": "none", "codex": "none", "gemini": "none",
        "hermes": "none", "opencode": "none", "zed": "none", "direnv": "none",
        "configMounts": [], "socksProxy": null, "allowHostLoopback": false,
        "passEnv": ["HOME", "PATH", "TERM", "DISPLAY", "WAYLAND_DISPLAY", "LANG"],
        "opensnitch": {"enable": false, "directory": "~/.local/share/sandboxes/s/opensnitch", "rules": null},
        "files": {}, "bin": []
    })
}

/// Merge `patch` into `base` (objects only).
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

/// A configuration with the sandbox "s" and `patch` for it.
pub fn config(patch: Value) -> Config {
    config_top(patch, json!({}))
}

/// The same, with `top` for the top level of config.json.
pub fn config_top(patch: Value, top: Value) -> Config {
    let mut e = env_json();
    merge(&mut e, &patch);
    let mut c = json!({
        "menu": {"command": "menu", "markup": false},
        "host": {"color": "#404040"},
        "backends": {"bwrap": "/b/bwrap", "gvisor": "/b/gvisor"},
        "init": "/i/sandbox-init",
        "sshConfig": null,
        "homeCacheDirs": [".cache/pip"],
        "commonHomeFolders": [".cargo"],
        "opensnitch": null,
        "environments": {"s": e},
    });
    merge(&mut c, &top);
    serde_json::from_value(c).expect("the test configuration")
}


/// The tests that catch, raise or take signals hold this lock. The flags of
/// catch_signals are of the process: without the lock, the proxy of a pty
/// test can take a signal that another test raised, and give it to its
/// child.
pub fn signals() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
