#[macro_use] extern crate log;

use std::fs::File;
use std::io::{self, Read, Write};
use std::mem;
use std::os::unix::io::{FromRawFd, AsRawFd, RawFd};
use std::process::exit;

use mio::{Events, Poll, Ready, PollOpt, Token};
use mio::unix::{EventedFd, UnixReady};

mod delay;
mod pty;
mod term;

use delay::Delay;

pub fn checkerr(result: i32, msg: &str) -> io::Result<i32> {
    if result == -1 {
        let e = io::Error::last_os_error();
        error!("{}: {}", msg, e);
        Err(e)
    } else {
        Ok(result)
    }
}

fn set_nonblocking(f: &mut File) -> io::Result<()> {
    let fd = f.as_raw_fd();
    let previous = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if previous < 0 {
        return Err(io::Error::last_os_error());
    }
    let new = previous | libc::O_NONBLOCK;
    if unsafe { libc::fcntl(fd, libc::F_SETFL, new) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn signal_name(n: i32) -> String {
    extern "C" { fn strsignal(sig: libc::c_int) -> *const libc::c_char; }
    let ptr = unsafe { strsignal(n) };
    if ptr.is_null() {
        format!("Unknown signal {}", n)
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
    assert!(!signal_name(9).contains("9"));
    assert!(signal_name(-1).contains("-1"));
    assert!(signal_name(0).contains("0"));
    assert!(signal_name(999).contains("999"));
}

struct ForkResult {
    child_pid: libc::pid_t,
    pty_master: File,
    pty_slave: Option<File>,
}

fn setup() -> io::Result<ForkResult> {
    let window_size = term::WindowSize::from_fd(0)?;

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

fn main() {
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
            eprintln!("error: invalid number for the rate: {}", e);
            exit(2);
        });
    if rate <= 0. {
        eprintln!("error: rate must be greater than zero.");
        exit(2);
    }
    let delay = Delay::from_rate(rate);

    let mut console = unsafe { File::from_raw_fd(0) };
    let ForkResult { child_pid, mut pty_master, pty_slave } = setup().unwrap();

    event_loop(delay, &mut console, &mut pty_master);

    debug!("dropping pty fds");
    mem::drop(pty_master);
    mem::drop(pty_slave);

    debug!("waiting on child");
    let mut child_status = 0;
    checkerr(unsafe { libc::waitpid(child_pid, &mut child_status, 0) }, "waitpid")
        .unwrap();

    debug!("resetting tty settings");
    term::reset_tty();

    if child_status != 0 {
        let exit_code = unsafe {
            if libc::WIFEXITED(child_status) {
                let exit_code = libc::WEXITSTATUS(child_status);
                error!("child exited with {}", exit_code);
                exit_code
            } else if libc::WIFSIGNALED(child_status) {
                let sig = libc::WTERMSIG(child_status);
                let name = signal_name(sig);
                error!("child killed by signal: {}", name);
                128 + sig
            } else {
                error!("something happened to the child, status {}", child_status);
                -1
            }
        };
        std::process::exit(exit_code);
    } else {
        debug!("child exited cleanly");
    }

    debug!("returning from main");
}

fn event_loop<'a>(delay: Delay, mut console: &'a mut File, mut pty_master: &'a mut File) {
    let poll = Poll::new().unwrap();
    for (i, f) in [&mut console, &mut pty_master].iter_mut().enumerate() {
        set_nonblocking(f).unwrap();
        poll.register(
                &EventedFd(&f.as_raw_fd()),
                Token(i),
                Ready::readable() | UnixReady::error() | UnixReady::hup(),
                PollOpt::level())
            .unwrap();
    }

    let names = ["console", "pty"];
    let mut events = Events::with_capacity(1024);
    loop {
        poll.poll(&mut events, None).unwrap();
        debug!("poll returned");

        for event in &events {
            debug!("{:?}", event);

            let readiness = UnixReady::from(event.readiness());
            if readiness.is_hup() && !readiness.is_readable() {
                // Don't try to read in this state. Even with O_NONBLOCK set, it may still block.
                debug!("breaking out");
                return;
            }

            let index = event.token().0 as usize;
            let (source, dest) = if index == 0 {
                (&mut console, &mut pty_master)
            } else {
                (&mut pty_master, &mut console)
            };

            let mut buf = [0u8];

            match source.read(&mut buf) {
                Ok(0) => {
                    debug!("zero bytes from {}", names[index]);
                    return;
                }
                Ok(1) => {
                    debug!("got {:?}", buf[0] as char);

                    if let Err(e) = dest.write_all(&buf) {
                        error!("write error: {}", e);
                        return;
                    }

                    delay.sleep()
                        .unwrap_or_else(|e| {
                            error!("delay error: {}", e);
                        });
                }
                Ok(_) => unreachable!(),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Spurious event; ignore and continue.
                    debug!("wouldblock from {}", names[index]);
                }
                Err(ref e) => {
                    panic!("read error {}", e);
                }
            }
        }
    }
}
