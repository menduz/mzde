//! Small functions for all parts of the command.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

/// The name of the program in messages.
pub const PROG: &str = "sandbox";

/// Print `sandbox: MESSAGE` and stop with exit code 1.
#[macro_export]
macro_rules! die {
    ($($arg:tt)*) => {{
        eprintln!("{}: {}", $crate::util::PROG, format!($($arg)*));
        std::process::exit(1)
    }};
}

/// Print `sandbox: MESSAGE`.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        eprintln!("{}: {}", $crate::util::PROG, format!($($arg)*))
    };
}

/// An error with a message for the user.
pub type Result<T> = std::result::Result<T, String>;

/// The value of an environment variable, if it is set and valid UTF-8.
pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// The home directory of the user (HOME).
pub fn home() -> String {
    env("HOME").unwrap_or_else(|| die!("HOME is not set"))
}

/// Expand a "~" at the start of a path to $HOME, and remove the empty
/// segments: "~/code/" and "~//code" are $HOME/code. The tests of the current
/// directory compare the strings.
pub fn expand(p: &str, home: &str) -> String {
    let p = if p == "~" {
        home.to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{home}/{rest}")
    } else {
        p.to_string()
    };
    normalize(&p)
}

/// Remove the empty segments of a path and a "/" at the end.
pub fn normalize(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut last_slash = false;
    for c in p.chars() {
        if c == '/' {
            if last_slash {
                continue;
            }
            last_slash = true;
        } else {
            last_slash = false;
        }
        out.push(c);
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// Whether a path exists (after links), as `test -e`.
pub fn exists(p: &str) -> bool {
    fs::metadata(p).is_ok()
}

/// Whether a path is a socket (after links), as `test -S`.
pub fn is_socket(p: &str) -> bool {
    fs::metadata(p).map(|m| m.file_type().is_socket()).unwrap_or(false)
}

/// The path with all links resolved, as `readlink -f`: the last component
/// can be missing.
pub fn readlink_f(p: &str) -> String {
    if let Ok(c) = fs::canonicalize(p) {
        return path_str(&c);
    }
    let path = Path::new(p);
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => match fs::canonicalize(if dir.as_os_str().is_empty() { Path::new(".") } else { dir }) {
            Ok(d) => path_str(&d.join(name)),
            Err(_) => p.to_string(),
        },
        _ => p.to_string(),
    }
}

/// A path as a string. The paths of the command are valid UTF-8.
pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The directory as `$PWD` of a shell: PWD if it is the current directory
/// (it can contain links), else the current directory without links.
pub fn logical_pwd() -> String {
    let real = std::env::current_dir().map(|p| path_str(&p)).unwrap_or_else(|_| "/".into());
    choose_pwd(env("PWD").as_deref(), real, |p| same_file(p, "."))
}

/// The `$PWD` of logical_pwd: `pwd` without empty segments, if `here` tells
/// that it is the current directory, else `real`. The tests of the current
/// directory compare the strings with the paths of the sandbox, so `pwd`
/// must not have a "." or ".." segment: then `real`.
fn choose_pwd(pwd: Option<&str>, real: String, here: impl Fn(&str) -> bool) -> String {
    if let Some(pwd) = pwd
        && pwd.starts_with('/')
        && !pwd.split('/').any(|s| s == "." || s == "..")
        && here(pwd)
    {
        return normalize(pwd);
    }
    real
}

/// Whether two paths are the same file (device and inode).
pub fn same_file(a: &str, b: &str) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// The real user ID and group ID.
pub fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

pub fn gid() -> u32 {
    // SAFETY: getgid cannot fail.
    unsafe { libc::getgid() }
}

/// The directory for the files of the command: the runtime directory, else
/// /tmp.
pub fn files_dir() -> String {
    env("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()).unwrap_or_else(|| "/tmp".into())
}


/// Whether standard input and standard output are terminals.
pub fn has_tty() -> bool {
    // SAFETY: isatty only reads the descriptor.
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

/// The start time of the process `pid` (field 22 of /proc/PID/stat), so
/// that a different process with the same PID is not the process of a file.
/// The name of the program (field 2) can have spaces, thus remove it first.
pub fn starttime(pid: i32) -> Option<String> {
    let stat = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.iter().rposition(|&c| c == b')')?;
    let rest = OsStr::from_bytes(&stat[close + 1..]).to_string_lossy().into_owned();
    rest.split_whitespace().nth(19).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_paths() {
        let h = "/home/a";
        assert_eq!(expand("~", h), "/home/a");
        assert_eq!(expand("~/code/", h), "/home/a/code");
        assert_eq!(expand("~//code", h), "/home/a/code");
        assert_eq!(expand("/", h), "/");
        assert_eq!(expand("//x//y/", h), "/x/y");
        assert_eq!(expand("~x", h), "~x");
    }

    #[test]
    fn pwd_without_empty_or_dot_segments() {
        let here = |_: &str| true;
        let pwd = |p: &str| choose_pwd(Some(p), "/real".into(), here);
        assert_eq!(pwd("/a/x"), "/a/x");
        assert_eq!(pwd("/a//x"), "/a/x");
        assert_eq!(pwd("//a/x/"), "/a/x");
        assert_eq!(pwd("/"), "/");
        assert_eq!(pwd("/a/..x/.y"), "/a/..x/.y");
        // A "." or ".." segment: the current directory without links.
        assert_eq!(pwd("/a/./x"), "/real");
        assert_eq!(pwd("/a/y/../x"), "/real");
        assert_eq!(pwd("/a/x/.."), "/real");
        assert_eq!(pwd("a/x"), "/real");
        assert_eq!(choose_pwd(Some("/a/x"), "/real".into(), |_| false), "/real");
        assert_eq!(choose_pwd(None, "/real".into(), here), "/real");
    }

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize("/a//b/"), "/a/b");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("//"), "/");
        assert_eq!(normalize("a/"), "a");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn readlink_f_resolves_links_and_a_missing_last_component() {
        let d = std::env::temp_dir().join(format!("sandbox-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("real")).unwrap();
        fs::write(d.join("real/file"), "").unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        std::os::unix::fs::symlink(d.join("real/file"), d.join("filelink")).unwrap();
        let real = fs::canonicalize(&d).unwrap();
        let p = |x: &str| d.join(x).to_str().unwrap().to_string();
        assert_eq!(readlink_f(&p("filelink")), real.join("real/file").to_str().unwrap());
        assert_eq!(readlink_f(&p("link/file")), real.join("real/file").to_str().unwrap());
        // The last component can be missing; its directory is resolved.
        assert_eq!(readlink_f(&p("link/new")), real.join("real/new").to_str().unwrap());
        // Two missing components: the path as it is.
        assert_eq!(readlink_f(&p("missing/new")), p("missing/new"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn files_and_sockets() {
        let d = std::env::temp_dir().join(format!("sandbox-sock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let sock = d.join("s");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let s = sock.to_str().unwrap();
        assert!(exists(s) && is_socket(s));
        let dir = d.to_str().unwrap();
        assert!(exists(dir) && !is_socket(dir));
        assert!(!exists(&format!("{dir}/none")) && !is_socket(&format!("{dir}/none")));
        assert!(same_file(dir, &format!("{dir}/.")));
        assert!(!same_file(dir, s));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn starttime_of_self() {
        let pid = std::process::id() as i32;
        let t = starttime(pid).unwrap();
        assert!(t.chars().all(|c| c.is_ascii_digit()), "{t}");
    }
}
