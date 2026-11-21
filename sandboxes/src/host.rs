//! What the plan reads from the host, and what it changes on the host. The
//! plan (plan.rs) uses only this interface, so that a test can give it a
//! host of its own (`Fake`).

use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

use crate::util::{self, Result};

pub trait Host {
    /// An environment variable of the caller.
    fn var(&self, name: &str) -> Option<String>;
    /// Whether a path exists (after links), as `test -e`.
    fn exists(&self, path: &str) -> bool;
    /// Whether a path is a socket (after links), as `test -S`.
    fn is_socket(&self, path: &str) -> bool;
    /// The path with all links resolved, as `readlink -f`.
    fn readlink_f(&self, path: &str) -> String;
    /// The socket directory of gpg-agent (`gpgconf --list-dirs socketdir`).
    fn gpg_socketdir(&self, run: &str) -> String;
    /// The devices in /dev whose names start with `prefix`, sorted.
    fn devices(&self, prefix: &str) -> Vec<String>;
    /// The current directory, as $PWD of a shell.
    fn pwd(&self) -> String;
    /// Whether standard input and standard output are terminals.
    fn tty(&self) -> bool;
    fn uid(&self) -> u32;
    fn gid(&self) -> u32;
    fn random(&self) -> u32;
    /// Make a directory and its parents, with the mode 0700.
    fn private_dir(&self, path: &str) -> Result<()>;
    /// Make a directory and its parents.
    fn make_dir(&self, path: &str) -> Result<()>;
    /// /tmp/sandbox-UID-NAME, a directory of this user with the mode 0700.
    fn shared_tmp(&self, name: &str) -> Result<String>;
    /// A message for the user.
    fn warn(&self, msg: &str);

    /// A variable that is set and not empty.
    fn var_nonempty(&self, name: &str) -> Option<String> {
        self.var(name).filter(|v| !v.is_empty())
    }

    /// The home directory of the user.
    fn home(&self) -> Result<String> {
        self.var("HOME").ok_or_else(|| "HOME is not set".to_string())
    }

    /// The runtime directory of the user.
    fn runtime_dir(&self) -> String {
        self.var("XDG_RUNTIME_DIR").unwrap_or_else(|| format!("/run/user/{}", self.uid()))
    }

    /// The path on the host of the path `p` below $HOME. A path below
    /// .config, .local/share or .cache is in the XDG directory of the host.
    fn host_path(&self, p: &str, home: &str) -> String {
        let xdg = |var: &str, default: &str| self.var(var).unwrap_or_else(|| format!("{home}/{default}"));
        if let Some(rest) = p.strip_prefix(".config/") {
            format!("{}/{rest}", xdg("XDG_CONFIG_HOME", ".config"))
        } else if let Some(rest) = p.strip_prefix(".local/share/") {
            format!("{}/{rest}", xdg("XDG_DATA_HOME", ".local/share"))
        } else if let Some(rest) = p.strip_prefix(".cache/") {
            format!("{}/{rest}", xdg("XDG_CACHE_HOME", ".cache"))
        } else {
            format!("{home}/{p}")
        }
    }
}

/// The real host: this process and its file system.
pub struct System;

impl Host for System {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
    }

    fn exists(&self, path: &str) -> bool {
        util::exists(path)
    }

    fn is_socket(&self, path: &str) -> bool {
        util::is_socket(path)
    }

    fn readlink_f(&self, path: &str) -> String {
        util::readlink_f(path)
    }

    fn gpg_socketdir(&self, run: &str) -> String {
        let out = std::process::Command::new(crate::tools::gpgconf())
            .args(["--list-dirs", "socketdir"])
            .stderr(std::process::Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim_end_matches('\n').to_string();
                if s.is_empty() { format!("{run}/gnupg") } else { s }
            }
            _ => format!("{run}/gnupg"),
        }
    }

    fn devices(&self, prefix: &str) -> Vec<String> {
        let mut devs: Vec<String> = fs::read_dir("/dev")
            .map(|d| {
                d.filter_map(|x| x.ok())
                    .map(|x| x.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with(prefix))
                    .map(|n| format!("/dev/{n}"))
                    .collect()
            })
            .unwrap_or_default();
        devs.sort();
        devs
    }

    fn pwd(&self) -> String {
        util::logical_pwd()
    }

    fn tty(&self) -> bool {
        util::has_tty()
    }

    fn uid(&self) -> u32 {
        util::uid()
    }

    fn gid(&self) -> u32 {
        util::gid()
    }

    fn random(&self) -> u32 {
        let mut b = [0u8; 4];
        // SAFETY: the buffer has 4 bytes.
        let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), 4, 0) };
        if n == 4 { u32::from_ne_bytes(b) } else { std::process::id() }
    }

    fn private_dir(&self, p: &str) -> Result<()> {
        fs::create_dir_all(p).map_err(|e| format!("cannot make {p}: {e}"))?;
        fs::set_permissions(p, fs::Permissions::from_mode(0o700)).map_err(|e| format!("{p}: {e}"))
    }

    fn make_dir(&self, p: &str) -> Result<()> {
        fs::create_dir_all(p).map_err(|e| format!("cannot make {p}: {e}"))
    }

    fn shared_tmp(&self, name: &str) -> Result<String> {
        shared_tmp_in("/tmp", self.uid(), name)
    }

    fn warn(&self, msg: &str) {
        crate::warn!("{msg}");
    }
}

/// DIR/sandbox-UID-NAME: one /tmp for all runs of the sandbox. A different
/// user can make this path first, so make sure that it is a directory of
/// this user, and not a link.
pub fn shared_tmp_in(dir: &str, uid: u32, name: &str) -> Result<String> {
    let shared = format!("{dir}/sandbox-{uid}-{name}");
    let _ = fs::DirBuilder::new().mode(0o700).create(&shared);
    let bad = || format!("{shared} is not a directory of this user");
    let m = fs::symlink_metadata(&shared).map_err(|_| bad())?;
    if m.file_type().is_symlink() || !m.is_dir() || m.uid() != uid {
        return Err(bad());
    }
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o700)).map_err(|e| format!("{shared}: {e}"))?;
    Ok(shared)
}

/// A host for tests: variables, files, sockets and links in tables.
#[cfg(test)]
pub mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    pub struct Fake {
        pub vars: BTreeMap<String, String>,
        pub files: BTreeSet<String>,
        pub sockets: BTreeSet<String>,
        pub links: BTreeMap<String, String>,
        pub gpg: Option<String>,
        pub devs: Vec<String>,
        pub cwd: String,
        pub terminal: bool,
        pub rand: u32,
        /// shared_tmp fails with this message.
        pub shared_tmp_error: Option<String>,
        pub made: RefCell<Vec<String>>,
        pub warnings: RefCell<Vec<String>>,
    }

    impl Fake {
        /// A host with HOME=/home/a, XDG_RUNTIME_DIR=/run/user/1000,
        /// PATH=/usr/bin, the current directory /home/a, and the base
        /// system (/nix/store, /etc, /bin, /usr, /run/current-system).
        pub fn new() -> Fake {
            let mut f = Fake {
                cwd: "/home/a".into(),
                rand: 7,
                ..Default::default()
            };
            f.set("HOME", "/home/a").set("XDG_RUNTIME_DIR", "/run/user/1000").set("PATH", "/usr/bin");
            for p in ["/nix/store", "/etc", "/bin", "/usr", "/run/current-system", "/home/a", "/tmp"] {
                f.files.insert(p.into());
            }
            f
        }

        pub fn set(&mut self, k: &str, v: &str) -> &mut Fake {
            self.vars.insert(k.into(), v.into());
            self
        }

        pub fn file(&mut self, p: &str) -> &mut Fake {
            self.files.insert(p.into());
            self
        }

        pub fn socket(&mut self, p: &str) -> &mut Fake {
            self.files.insert(p.into());
            self.sockets.insert(p.into());
            self
        }

        pub fn link(&mut self, from: &str, to: &str) -> &mut Fake {
            self.files.insert(from.into());
            self.files.insert(to.into());
            self.links.insert(from.into(), to.into());
            self
        }
    }

    impl Host for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }
        // A "/" at the end names the same file, as on a file system.
        fn exists(&self, path: &str) -> bool {
            self.files.contains(path) || self.files.contains(path.trim_end_matches('/'))
        }
        fn is_socket(&self, path: &str) -> bool {
            self.sockets.contains(path.trim_end_matches('/'))
        }
        fn readlink_f(&self, path: &str) -> String {
            self.links.get(path).cloned().unwrap_or_else(|| path.into())
        }
        fn gpg_socketdir(&self, run: &str) -> String {
            self.gpg.clone().unwrap_or_else(|| format!("{run}/gnupg"))
        }
        fn devices(&self, prefix: &str) -> Vec<String> {
            let mut d: Vec<String> = self.devs.iter().filter(|d| d.starts_with(&format!("/dev/{prefix}"))).cloned().collect();
            d.sort();
            d
        }
        fn pwd(&self) -> String {
            self.cwd.clone()
        }
        fn tty(&self) -> bool {
            self.terminal
        }
        fn uid(&self) -> u32 {
            1000
        }
        fn gid(&self) -> u32 {
            100
        }
        fn random(&self) -> u32 {
            self.rand
        }
        fn private_dir(&self, p: &str) -> Result<()> {
            self.made.borrow_mut().push(format!("private {p}"));
            Ok(())
        }
        fn make_dir(&self, p: &str) -> Result<()> {
            self.made.borrow_mut().push(format!("dir {p}"));
            Ok(())
        }
        fn shared_tmp(&self, name: &str) -> Result<String> {
            match &self.shared_tmp_error {
                Some(e) => Err(e.clone()),
                None => Ok(format!("/tmp/sandbox-1000-{name}")),
            }
        }
        fn warn(&self, msg: &str) {
            self.warnings.borrow_mut().push(msg.into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> String {
        let d = std::env::temp_dir().join(format!("sandbox-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().into_owned()
    }

    #[test]
    fn shared_tmp_makes_a_private_directory() {
        let d = tempdir("shared");
        let uid = util::uid();
        let p = shared_tmp_in(&d, uid, "x").unwrap();
        assert_eq!(p, format!("{d}/sandbox-{uid}-x"));
        let m = fs::metadata(&p).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o700);
        // A second call finds the same directory, and makes it 0700 again.
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(shared_tmp_in(&d, uid, "x").unwrap(), p);
        assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o700);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn shared_tmp_refuses_a_link_a_file_and_another_owner() {
        let d = tempdir("shared-bad");
        let uid = util::uid();
        let target = format!("{d}/elsewhere");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, format!("{d}/sandbox-{uid}-link")).unwrap();
        assert!(shared_tmp_in(&d, uid, "link").unwrap_err().contains("is not a directory of this user"));
        fs::write(format!("{d}/sandbox-{uid}-file"), "").unwrap();
        assert!(shared_tmp_in(&d, uid, "file").is_err());
        // A directory of a different user: here, the uid that is not ours.
        fs::create_dir(format!("{d}/sandbox-{}-other", uid + 1)).unwrap();
        assert!(shared_tmp_in(&d, uid + 1, "other").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn host_path_uses_the_xdg_directories() {
        let mut f = fake::Fake::new();
        assert_eq!(f.host_path(".config/zed/", "/home/a"), "/home/a/.config/zed/");
        assert_eq!(f.host_path(".gitconfig", "/home/a"), "/home/a/.gitconfig");
        f.set("XDG_CONFIG_HOME", "/x/conf").set("XDG_DATA_HOME", "/x/data").set("XDG_CACHE_HOME", "/x/cache");
        assert_eq!(f.host_path(".config/zed/", "/home/a"), "/x/conf/zed/");
        assert_eq!(f.host_path(".local/share/zed/", "/home/a"), "/x/data/zed/");
        assert_eq!(f.host_path(".cache/pip", "/home/a"), "/x/cache/pip");
        // Only below the directory: .configx is not .config.
        assert_eq!(f.host_path(".configx", "/home/a"), "/home/a/.configx");
    }

    #[test]
    fn runtime_dir_default() {
        let mut f = fake::Fake::new();
        assert_eq!(f.runtime_dir(), "/run/user/1000");
        f.vars.remove("XDG_RUNTIME_DIR");
        assert_eq!(f.runtime_dir(), "/run/user/1000");
        f.set("XDG_RUNTIME_DIR", "/r");
        assert_eq!(f.runtime_dir(), "/r");
    }

    #[test]
    fn system_devices_are_sorted() {
        let d = System.devices("tty");
        let mut s = d.clone();
        s.sort();
        assert_eq!(d, s);
        assert!(d.iter().all(|p| p.starts_with("/dev/tty")));
    }
}
