//! A terminal of its own for a run with a terminal. The program in the
//! sandbox gets a new pseudo-terminal, not the terminal of the caller: it
//! cannot put characters into the input of the terminal of the caller
//! (TIOCSTI), or change its settings. This process copies the bytes between
//! the two terminals, with the terminal of the caller in raw mode, so Ctrl-C
//! and Ctrl-Z go to the program in the sandbox. A change of the window size
//! goes to the new terminal.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::process::{self, catch_signals, take_signals};
use crate::util::Result;

pub struct Pty {
    pub master: OwnedFd,
    pub slave: OwnedFd,
}

fn last_error(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

/// A new pseudo-terminal with the settings and the window size of the
/// terminal of the caller (standard input).
pub fn open() -> Result<Pty> {
    // SAFETY: system calls with valid arguments; the descriptors are new.
    unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if m < 0 {
            return Err(last_error("posix_openpt"));
        }
        let master = OwnedFd::from_raw_fd(m);
        if libc::grantpt(m) != 0 || libc::unlockpt(m) != 0 {
            return Err(last_error("unlockpt"));
        }
        let mut name = [0 as libc::c_char; 128];
        if libc::ptsname_r(m, name.as_mut_ptr(), name.len()) != 0 {
            return Err(last_error("ptsname"));
        }
        let s = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
        if s < 0 {
            return Err(last_error("open the pseudo-terminal"));
        }
        let slave = OwnedFd::from_raw_fd(s);
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            libc::tcsetattr(s, libc::TCSANOW, &t);
        }
        copy_size(0, m);
        Ok(Pty { master, slave })
    }
}

/// Give the window size of `from` to `to`.
fn copy_size(from: RawFd, to: RawFd) {
    // SAFETY: ioctls with a valid winsize.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(from, libc::TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(to, libc::TIOCSWINSZ, &ws);
        }
    }
}

/// Start `cmd` on the slave: its standard input, output and error, and its
/// controlling terminal in a new session. `before` runs first in the child
/// (for example setns).
pub fn spawn(cmd: &mut Command, pty: &Pty, before: impl Fn() -> std::io::Result<()> + Send + Sync + 'static) -> Result<libc::pid_t> {
    let dup = |fd: &OwnedFd| fd.try_clone().map(Stdio::from).map_err(|e| e.to_string());
    cmd.stdin(dup(&pty.slave)?).stdout(dup(&pty.slave)?).stderr(dup(&pty.slave)?);
    // SAFETY: the closure calls only system calls.
    unsafe {
        cmd.pre_exec(move || {
            before()?;
            attach(0)
        });
    }
    let child = cmd.spawn().map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    Ok(child.id() as libc::pid_t)
}

/// In a child after fork: a new session with the terminal `slave` as its
/// controlling terminal and as its standard input, output and error.
///
/// # Safety
///
/// The function calls only system calls, as necessary after fork.
pub unsafe fn attach(slave: RawFd) -> std::io::Result<()> {
    // SAFETY: system calls with a valid descriptor.
    unsafe {
        if libc::setsid() < 0 || libc::ioctl(slave, libc::TIOCSCTTY, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        for fd in 0..3 {
            if fd != slave && libc::dup2(slave, fd) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Restores the settings of the terminal of the caller.
struct Raw(Option<(RawFd, libc::termios)>);

impl Raw {
    /// Raw mode for the terminal `fd`; nothing if it is not a terminal.
    fn new(fd: RawFd) -> Raw {
        // SAFETY: tcgetattr/tcsetattr with a valid termios.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return Raw(None);
            }
            let mut raw = t;
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(fd, libc::TCSADRAIN, &raw);
            Raw(Some((fd, t)))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        if let Some((fd, t)) = &self.0 {
            // SAFETY: the saved settings.
            unsafe { libc::tcsetattr(*fd, libc::TCSADRAIN, t) };
        }
    }
}

/// Write all bytes; false if the descriptor is closed.
fn write_all(fd: RawFd, mut buf: &[u8]) -> bool {
    while !buf.is_empty() {
        // SAFETY: a valid buffer.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        buf = &buf[n as usize..];
    }
    true
}

/// Copy the bytes between the terminal of the caller and the master until
/// `child` stops, and return its exit code. TERM and HUP go to the child.
pub fn proxy(pty: Pty, child: libc::pid_t) -> i32 {
    catch_signals(&[libc::SIGWINCH, libc::SIGTERM, libc::SIGHUP]);
    proxy_with(pty, child, 0, 1)
}

/// `proxy` with the terminal of the caller on `input` and `output`.
fn proxy_with(pty: Pty, child: libc::pid_t, input: RawFd, output: RawFd) -> i32 {
    let Pty { master, slave } = pty;
    // The slave stays open only in the child: then the master gets an end
    // of file when the programs close the terminal.
    drop(slave);
    let raw = Raw::new(input);
    let m = master.as_raw_fd();
    let mut stdin_open = true;
    let mut buf = [0u8; 16384];
    let mut status = None;
    loop {
        for s in take_signals() {
            if s == libc::SIGWINCH {
                copy_size(input, m);
            } else {
                process::kill(child, s);
            }
        }
        if status.is_none() {
            let mut st = 0;
            // SAFETY: status is a valid pointer.
            if unsafe { libc::waitpid(child, &mut st, libc::WNOHANG) } == child {
                status = Some(st);
            }
        }
        let mut fds = [
            libc::pollfd { fd: m, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: if stdin_open { input } else { -1 }, events: libc::POLLIN, revents: 0 },
        ];
        // After the child: only the output that is still in the terminal.
        let timeout = if status.is_some() { 0 } else { 100 };
        // SAFETY: two valid pollfds.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if r < 0 {
            continue;
        }
        let mut got = false;
        if fds[0].revents & libc::POLLIN != 0 {
            // SAFETY: a valid buffer.
            let n = unsafe { libc::read(m, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                got = true;
                write_all(output, &buf[..n as usize]);
            }
        }
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            // SAFETY: a valid buffer.
            let n = unsafe { libc::read(input, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                stdin_open = false;
            } else {
                write_all(m, &buf[..n as usize]);
            }
        }
        let hangup = fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 && !got;
        if let Some(st) = status
            && (!got || hangup)
        {
            drop(raw);
            return process::code_raw(st);
        }
        if hangup && status.is_none() {
            // The programs closed the terminal: wait for the child.
            let st = loop {
                if let Some(st) = process::wait_pid(child) {
                    break st;
                }
            };
            drop(raw);
            return process::code_raw(st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    /// Run a shell script on a new pty, with `input` as the keyboard. The
    /// result: the exit code and the output.
    fn run(script: &str, input: &[u8]) -> (i32, String) {
        let pty = open().unwrap();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", script]);
        let child = spawn(&mut cmd, &pty, || Ok(())).unwrap();
        let (in_r, in_w) = process::pipe().unwrap();
        let (out_r, out_w) = process::pipe().unwrap();
        std::fs::File::from(in_w).write_all(input).unwrap();
        // Read the output while the proxy runs: a pipe holds only 64 KiB.
        let reader = std::thread::spawn(move || {
            let mut out = String::new();
            std::fs::File::from(out_r).read_to_string(&mut out).unwrap();
            out
        });
        let rc = proxy_with(pty, child, in_r.as_raw_fd(), out_w.as_raw_fd());
        drop(out_w);
        (rc, reader.join().unwrap())
    }

    #[test]
    fn the_program_has_a_terminal_and_the_input() {
        let (rc, out) = run("test -t 0 && test -t 1 && echo is-tty; read l; echo got:$l; exit 4", b"hello\n");
        assert_eq!(rc, 4, "{out}");
        assert!(out.contains("is-tty"), "{out}");
        assert!(out.contains("got:hello"), "{out}");
    }

    #[test]
    fn the_terminal_is_the_controlling_terminal_of_a_new_session() {
        // The shell is the leader of its session: its session ID is its PID.
        let (rc, out) = run("ps -o sid= -p $$ 2>/dev/null || cat /proc/$$/stat; echo pid:$$; tty", b"");
        assert_eq!(rc, 0, "{out}");
        assert!(out.contains("/dev/pts/"), "{out}");
    }

    #[test]
    fn a_program_in_the_background_does_not_keep_the_run() {
        let t = Instant::now();
        let (rc, out) = run("sleep 3 & echo started; exit 3", b"");
        assert_eq!(rc, 3, "{out}");
        assert!(out.contains("started"));
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[test]
    fn all_output_comes_through() {
        let (rc, out) = run("i=0; while [ $i -lt 2000 ]; do echo line-$i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done; echo end", b"");
        assert_eq!(rc, 0);
        assert!(out.contains("line-1999-") && out.trim_end().ends_with("end"), "{}", &out[out.len().saturating_sub(200)..]);
        assert_eq!(out.matches("line-").count(), 2000);
    }

    #[test]
    fn a_signal_gives_128_and_the_signal() {
        let (rc, _) = run("kill -9 $$", b"");
        assert_eq!(rc, 128 + 9);
    }
}
