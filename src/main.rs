#[macro_use] extern crate anyhow;
#[macro_use] extern crate log;

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::io::{FromRawFd, AsRawFd, RawFd};
use std::process::exit;

mod delay;
mod pty;
mod readable;
mod term;

use delay::Delay;
use readable::{PollEndpoint, PollResult, ReadableSet};

pub fn checkerr(result: i32, msg: &'static str) -> Result<i32> {
    if result == -1 {
        let e = io::Error::last_os_error();
        Err(e).context(msg)
    } else {
        Ok(result)
    }
}

fn signal_name(n: i32) -> String {
    extern "C" { fn strsignal(sig: libc::c_int) -> *const libc::c_char; }
    let ptr = unsafe { strsignal(n) };
    if ptr.is_null() {
        format!("Unknown signal {n}")
    } else {
        let original = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy();

        if original.contains("Unknown") {
            // Usually of the form "Unknown signal <n>", so keep the original form.
            original.into_owned()
        } else {
            // Sometimes of the form "<signal name>: <n>", so strip off the colon and
            // everything after it.
            original
                .split(": ")
                .next()
                .unwrap()
                .to_owned()
        }
    }
}

#[test]
fn test_signal_name() {
    assert!(signal_name(libc::SIGKILL).to_lowercase().contains("kill"));
    assert!(!signal_name(9).contains('9'));
    assert!(signal_name(-1).contains("-1"));
    assert!(signal_name(0).contains('0'));
    assert!(signal_name(999).contains("999"));
}

struct ForkResult {
    child_pid: libc::pid_t,
    pty_master: File,
    pty_slave: Option<File>,
}

fn setup() -> Result<ForkResult> {
    let window_size = term::WindowSize::from_fd(0).context("failed to get terminal size")?;

    let pty::PtyPair { master, slave } = pty::open_pty_pair()?;

    let pid = checkerr(unsafe { libc::fork() }, "fork")?;
    if pid != 0 {
        // parent

        let returned_slave = if cfg!(target_os = "macos") {
            // On macOS, it's observed that when the child exits and closes its slave end, the pty
            // drops all buffered data unless we hold it open by keeping another FD to it.
            debug!("keeping the pty slave open");
            Some(slave)
        } else {
            // On Linux, keeping a FD of the slave open is actively harmful and prevents us from
            // getting a HUP signal or the pty master from returning EWOULDBLOCK at all, which
            // results in us hanging when the child exits.
            // Other unixes? Who knows; haven't tested it. But this behavior seems more reasonable.
            debug!("dropping the pty slave");
            mem::drop(slave);
            None
        };

        term::save_term_settings(0)?;
        term::set_raw(0)?;
        term::restore_term_settings_at_exit()?;
        Ok(ForkResult { 
            child_pid: pid,
            pty_master: master,
            pty_slave: returned_slave,
        })
    } else {
        // child

        mem::drop(master);
        let fd: RawFd = slave.as_raw_fd();
        unsafe {
            checkerr(libc::dup2(fd, 0), "dup2 slave -> 0")?;
            checkerr(libc::dup2(fd, 1), "dup2 slave -> 1")?;
            checkerr(libc::dup2(fd, 2), "dup2 slave -> 2")?;
        }
        mem::drop(slave);

        term::set_session_leader()?;
        term::set_controlling_tty(0)?;
        window_size.apply_to_fd(0)?;

        // exec the command

        let mut args = std::env::args_os().skip(2);
        let mut cmd = exec::Command::new(args.next().unwrap());
        for arg in args {
            cmd.arg(arg);
        }
        let e = cmd.exec();

        // If we get here, there's been an error launching the command.
        eprintln!("{}: {}", std::env::args().next().unwrap(), e);
        exit(101);
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3
        || args[1] == "--help"
        || args[1] == "-h"
    {
        eprintln!(concat!("slowpty (rust,mio) v", env!("CARGO_PKG_VERSION")));
        eprintln!("usage: {} <rate> <program> [<args>...]", args[0]);
        eprintln!("  run the given program, limiting I/O to the specified number of bytes per \
                  second.");
        exit(2);
    }

    let rate: f64 = args[1].parse()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid number for the rate: {e}");
            exit(2);
        });
    if rate <= 0. {
        eprintln!("error: rate must be greater than zero.");
        exit(2);
    }
    let delay = Delay::from_rate(rate);

    let mut console = unsafe { File::from_raw_fd(0) };
    let ForkResult { child_pid, mut pty_master, pty_slave } = setup()
        .context("failed to setup PTY")?;

    event_loop(delay, &mut console, &mut pty_master)?;

    debug!("dropping pty fds");
    mem::drop(pty_master);
    mem::drop(pty_slave);

    debug!("waiting on child");
    let mut child_status = 0;
    checkerr(unsafe { libc::waitpid(child_pid, &mut child_status, 0) }, "waitpid")
        .context("error waiting for child process")?;

    debug!("resetting tty settings");
    term::reset_tty();

    if child_status != 0 {
        let exit_code = if libc::WIFEXITED(child_status) {
            let child_exit = libc::WEXITSTATUS(child_status);
            error!("child exited with {}", child_exit);
            child_exit
        } else if libc::WIFSIGNALED(child_status) {
            let sig = libc::WTERMSIG(child_status);
            let name = signal_name(sig);
            error!("child killed by signal: {}", name);
            128 + sig
        } else {
            error!("something happened to the child, status {}", child_status);
            -1
        };
        std::process::exit(exit_code);
    } else {
        debug!("child exited cleanly");
    }

    debug!("returning from main");
    Ok(())
}

fn event_loop<'a>(delay: Delay, console: &'a mut File, pty_master: &'a mut File) -> Result<()> {
    let mut readable_set = ReadableSet::new(console, pty_master).expect("creating readable set");

    loop {
        if readable_set.is_empty() {
            // No readable endpoints. Stop the busy-polling and block until one of them becomes
            // ready.
            match readable_set.block().expect("blocking for events") {
                PollResult::Ok => (),
                PollResult::Closed => {
                    // One of the endpoints closed; no point in continuing.
                    debug!("bailing out");
                    return Ok(());
                }
            }
        }

        // At this point we have at least one readable endpoint. For fairness, always try to read
        // from both endpoints on each iteration, so that an intermittently-readable endpoint
        // doesn't get blocked by an always-readable one.

        let mut unset: Vec<usize> = vec![];
        for idx in 0 ..= 1 {
            let PollEndpoint { name, ref mut src, ref mut dst } = readable_set.endpoint(idx)
                .unwrap();

            let mut buf = [0u8];
            match src.read(&mut buf) {
                Ok(0) => {
                    debug!("{}: read zero bytes", name);
                    return Ok(());
                }
                Ok(1) => {
                    debug!("{}: got {:?}", name, buf[0] as char);

                    if buf[0] == 0x1B {
                        // HACK: for escape sequences, try and read another byte and send both at
                        // once if we get one immediately.
                        // This is because some fragile programs (like crossterm) if they see a
                        // single ESC by itself from a read() will immediately treat it as a
                        // keypress and not try to read more bytes and interpret an escape
                        // sequence.
                        let mut buf2 = [0u8];
                        if let Ok(1) = src.read(&mut buf2) {
                            let buf = [buf[0], buf2[0]];
                            if let Err(e) = dst.write_all(&buf) {
                                return Err(e).context("write error");
                            }
                            continue;
                        }
                    }

                    if let Err(e) = dst.write_all(&buf) {
                        return Err(e).context("write error");
                    }
                }
                Ok(_) => unreachable!(),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Done reading from this source.
                    debug!("{}: would block", name);
                    unset.push(idx);
                }
                Err(ref e) if e.raw_os_error() == Some(libc::EIO) => {
                    // Not sure exactly what causes this.
                    warn!("{}: EIO", name);
                    return Ok(());
                }
                Err(ref e) => {
                    panic!("{name}: read error: {e}");
                }
            }
        }

        for idx in unset {
            readable_set.unset(idx);
        }

        // This is a full-duplex connection: a read can happen from both endpoints for a single
        // delay cycle.
        delay.sleep().context("delay error")?;
    }
}
