//! The configuration of the sandboxes: /etc/sandboxes/config.json, which the
//! NixOS module writes (refer to module.nix). A missing field or a wrong
//! value is an error. A field that this version does not know is only a
//! warning: a container keeps the `sandbox` of its start, but it reads the
//! configuration of the system now, which a newer module can write.

use serde::Deserialize;
use std::collections::BTreeMap;

use crate::util::{self, Result};

/// The default configuration file. SANDBOX_CONFIG gives a different file.
pub const DEFAULT_PATH: &str = "/etc/sandboxes/config.json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub menu: Menu,
    pub host: Host,
    /// The backends: name → program.
    pub backends: BTreeMap<String, String>,
    /// sandbox-init: the first program in a sandbox.
    pub init: String,
    /// ssh_config without the Include of files of /nix/store.
    pub ssh_config: Option<String>,
    pub home_cache_dirs: Vec<String>,
    pub common_home_folders: Vec<String>,
    pub opensnitch: Option<Opensnitch>,
    /// The systemd package of the system: systemd-run and systemctl.
    #[serde(default)]
    pub systemd: Option<String>,
    /// The programs that run only in sandboxes: name → sandbox → command.
    #[serde(default)]
    pub commands: BTreeMap<String, BTreeMap<String, Command>>,
    pub environments: BTreeMap<String, Env>,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// A program that runs only in some sandboxes (`commands` of a sandbox).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Command {
    /// The program in the store.
    pub program: String,
    pub cwd_only: bool,
    pub bwrap_args: Vec<String>,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Menu {
    pub command: String,
    pub markup: bool,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Host {
    pub color: String,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Opensnitch {
    pub daemon: String,
    pub config: String,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// One sandbox. All fields of config.json are here, so that a wrong field
/// is an error; kwm and `sandbox list` read some of them as JSON.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Env {
    pub description: String,
    pub color: String,
    pub gtk_theme: Option<String>,
    pub backend: String,
    pub hostname: String,
    pub persistent_home: bool,
    pub home_directory: String,
    pub home_patterns: Vec<String>,
    pub paths: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub ssh_write: bool,
    pub gpg: bool,
    pub docker: bool,
    pub fuse: bool,
    pub gui: bool,
    pub x11: Service,
    pub dbus: Service,
    /// Forward the notifications of the private bus to the host
    /// (notify_host.rs).
    pub notifications: bool,
    /// Give the color scheme of the host to the private bus
    /// (color_scheme.rs).
    pub follow_color_scheme: bool,
    pub nvidia: bool,
    pub kvm: bool,
    pub audio: bool,
    pub libvirt: bool,
    pub nix: bool,
    pub allow_home: bool,
    pub internet_access: bool,
    pub lan_access: bool,
    pub allow_hosts: Vec<String>,
    pub disable_networking: bool,
    pub share_host_network: bool,
    pub mount_home_cache: bool,
    pub mount_common_home_folders: bool,
    pub mount_tmp: bool,
    pub shared_tmp: bool,
    pub clean_tmp: bool,
    pub privacy_filter: bool,
    pub yolo: bool,
    pub backend_args: BTreeMap<String, Vec<String>>,
    pub ssh: Access,
    pub git: Access,
    pub claude: Access,
    pub codex: Access,
    pub gemini: Access,
    pub hermes: Access,
    pub opencode: Access,
    pub zed: Access,
    pub direnv: Access,
    pub config_mounts: Vec<ConfigMount>,
    pub socks_proxy: Option<String>,
    pub allow_host_loopback: bool,
    pub pass_env: Vec<String>,
    pub opensnitch: EnvOpensnitch,
    /// Read-only files below $HOME: path → store path.
    pub files: BTreeMap<String, String>,
    /// The bin directories of the packages, in the order of `packages`.
    pub bin: Vec<String>,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// x11 and dbus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    None,
    Private,
    Host,
}

/// The access to the configuration of a program of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    Ro,
    Rw,
}

/// A path below $HOME of an access option.
#[derive(Debug, Deserialize)]
pub struct ConfigMount {
    pub mode: MountMode,
    pub path: String,
    /// With xdgRemap: the XDG directory that replaces `path` if it is on the
    /// host; else "".
    pub remap: String,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct EnvOpensnitch {
    pub enable: bool,
    pub directory: String,
    /// The directory of the rules of the configuration.
    pub rules: Option<String>,
    /// The fields that this version does not know (refer to `unknown`).
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_json::Value>,
}

/// The network of a sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Net {
    None,
    Host,
    Private,
}

impl Env {
    pub fn net(&self) -> Net {
        if self.disable_networking {
            Net::None
        } else if self.share_host_network {
            Net::Host
        } else {
            Net::Private
        }
    }
}

impl Config {
    /// Read the configuration of SANDBOX_CONFIG, or of the default path. A
    /// field that this version does not know is a warning.
    pub fn load() -> Result<Config> {
        let path = util::env("SANDBOX_CONFIG").unwrap_or_else(|| DEFAULT_PATH.to_string());
        let text = std::fs::read(&path).map_err(|e| format!("cannot read {path}: {e}"))?;
        let config: Config = serde_json::from_slice(&text).map_err(|e| format!("{path}: {e}"))?;
        let unknown = config.unknown();
        if !unknown.is_empty() {
            crate::warn!(
                "{path}: this version of sandbox does not know the fields {}; it ignores them. A running container keeps its version: sandbox stop NAME starts the new one.",
                unknown.join(", ")
            );
        }
        Ok(config)
    }

    /// The fields that this version does not know, as paths
    /// ("environments.NAME.field").
    pub fn unknown(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut add = |prefix: &str, m: &BTreeMap<String, serde_json::Value>| {
            out.extend(m.keys().map(|k| if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }));
        };
        add("", &self.unknown);
        add("menu", &self.menu.unknown);
        add("host", &self.host.unknown);
        if let Some(o) = &self.opensnitch {
            add("opensnitch", &o.unknown);
        }
        for (name, by_env) in &self.commands {
            for (env, c) in by_env {
                add(&format!("commands.{name}.{env}"), &c.unknown);
            }
        }
        for (name, e) in &self.environments {
            let at = format!("environments.{name}");
            add(&at, &e.unknown);
            add(&format!("{at}.opensnitch"), &e.opensnitch.unknown);
            for (i, m) in e.config_mounts.iter().enumerate() {
                add(&format!("{at}.configMounts.{i}"), &m.unknown);
            }
        }
        out
    }

    /// The sandbox NAME.
    pub fn env(&self, name: &str) -> Result<&Env> {
        self.environments
            .get(name)
            .ok_or_else(|| format!("no sandbox has the name '{name}'. Run 'sandbox list'."))
    }

    /// The name and the program of the backend of a sandbox. `over` (the
    /// value of SANDBOX_BACKEND) replaces the backend of the sandbox.
    /// "runsc" is the name of gVisor in sandboxed-agents.
    pub fn backend_for(&self, e: &Env, over: Option<String>) -> Result<(String, String)> {
        let mut name = over.unwrap_or_else(|| e.backend.clone());
        if !self.backends.contains_key(&name) && name == "runsc" {
            name = "gvisor".into();
        }
        match self.backends.get(&name) {
            Some(p) => Ok((name, p.clone())),
            None => Err(format!("no backend has the name '{name}'. Run 'sandbox backends'.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{config, env_json};
    use serde_json::json;

    fn parse(v: serde_json::Value) -> std::result::Result<Config, String> {
        serde_json::from_value(v).map_err(|e| e.to_string())
    }

    fn top(env: serde_json::Value) -> serde_json::Value {
        json!({
            "menu": {"command": "m", "markup": true},
            "host": {"color": "#404040"},
            "backends": {"bwrap": "/b"},
            "init": "/i",
            "sshConfig": null,
            "homeCacheDirs": [],
            "commonHomeFolders": [],
            "opensnitch": null,
            "environments": {"s": env},
        })
    }

    #[test]
    fn a_complete_configuration_parses() {
        let c = parse(top(env_json())).unwrap();
        assert!(c.menu.markup);
        assert_eq!(c.environments["s"].x11, Service::Private);
        assert_eq!(c.environments["s"].ssh, Access::None);
        // The optional fields of newer modules.
        assert!(c.systemd.is_none() && c.commands.is_empty());
    }

    #[test]
    fn an_unknown_field_is_only_a_warning() {
        // A newer module: a field of a sandbox, of a mount, of a command and
        // of the top level.
        let mut e = env_json();
        e["extraField"] = json!({"a": 1});
        e["configMounts"] = json!([{"mode": "ro", "path": ".a", "remap": "", "newer": true}]);
        let mut t = top(e);
        t["other"] = json!(1);
        t["commands"] = json!({"claude": {"s": {"program": "/p", "cwdOnly": false, "bwrapArgs": [], "x": 1}}});
        let c = parse(t).unwrap();
        assert_eq!(c.unknown(), ["other", "commands.claude.s.x", "environments.s.extraField", "environments.s.configMounts.0.newer"]);
        // The known fields keep their values.
        assert_eq!(c.environments["s"].x11, Service::Private);
        assert_eq!(c.environments["s"].config_mounts[0].mode, MountMode::Ro);
        assert!(parse(top(env_json())).unwrap().unknown().is_empty());
    }

    #[test]
    fn a_missing_field_or_a_wrong_value_is_an_error() {
        let mut e = env_json();
        e.as_object_mut().unwrap().remove("gui");
        assert!(parse(top(e)).unwrap_err().contains("missing field `gui`"));
        let mut e = env_json();
        e["x11"] = json!("sometimes");
        assert!(parse(top(e)).is_err());
        let mut e = env_json();
        e["ssh"] = json!(true);
        assert!(parse(top(e)).is_err(), "the module writes access values as strings");
        let mut e = env_json();
        e["configMounts"] = json!([{"mode": "rx", "path": ".a", "remap": ""}]);
        assert!(parse(top(e)).is_err());
    }

    #[test]
    fn network_of_a_sandbox() {
        let net = |p: serde_json::Value| config(p).environments["s"].net();
        assert_eq!(net(json!({})), Net::Private);
        assert_eq!(net(json!({"shareHostNetwork": true})), Net::Host);
        assert_eq!(net(json!({"disableNetworking": true, "shareHostNetwork": true})), Net::None);
    }

    #[test]
    fn the_backend_of_a_sandbox() {
        let c = crate::testutil::config_top(json!({}), json!({"backends": {"bwrap": "/b/bwrap", "gvisor": "/b/gvisor"}}));
        let e = &c.environments["s"];
        assert_eq!(c.backend_for(e, None).unwrap(), ("bwrap".into(), "/b/bwrap".into()));
        assert_eq!(c.backend_for(e, Some("gvisor".into())).unwrap().1, "/b/gvisor");
        assert_eq!(c.backend_for(e, Some("runsc".into())).unwrap().0, "gvisor");
        assert!(c.backend_for(e, Some("x".into())).unwrap_err().contains("no backend has the name 'x'"));
        assert!(c.env("nope").unwrap_err().contains("no sandbox has the name 'nope'"));
    }

    #[test]
    fn commands_parse() {
        let c = crate::testutil::config_top(json!({}), json!({"commands": {"claude": {"s": {"program": "/p", "cwdOnly": true, "bwrapArgs": ["--x"]}}}}));
        let cmd = &c.commands["claude"]["s"];
        assert!(cmd.cwd_only);
        assert_eq!(cmd.bwrap_args, ["--x"]);
    }
}
