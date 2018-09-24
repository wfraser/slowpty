extern crate env_logger;
extern crate exec;
extern crate libc;
#[macro_use] extern crate log;
extern crate mio;

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
    unsafe {
        let fd = f.as_raw_fd();
        let previous = libc::fcntl(fd, libc::F_GETFL);
        if previous < 0 {
            return Err(io::Error::last_os_error());
        }
        let new = previous | libc::O_NONBLOCK;
        if libc::fcntl(fd, libc::F_SETFL, new) < 0 {
            return Err(io::Error::last_os_error());
        }
	}
	Ok(())
}

struct ForkResult {
    child_pid: libc::pid_t,
    pty_master: File,
    pty_slave: File,
}

fn setup() -> io::Result<ForkResult> {
    let window_size = term::WindowSize::get_from_fd(0)?;

    let pty::PtyPair { master, slave } = pty::open_pty_pair()?;

    let pid = checkerr(unsafe { libc::fork() }, "fork")?;
    if pid != 0 {
        // parent

        // Normally, we'd close the slave end of the pty at this point, but on macOS it's observed
        // that when the child exits and closes its slave end, the pty drops all buffered input
        // unless we also hold it open by keeping another FD to it.
        // On Linux, we can use fcntl(F_SETPIPE_SZ, 1) to eliminate the buffer entirely, but macOS
        // does not support this fcntl, so we have to do this instead.
        //mem::drop(slave);

        term::save_term_settings(0)?;
        term::set_raw(0)?;
        term::restore_term_settings_at_exit()?;
        Ok(ForkResult { 
            child_pid: pid,
            pty_master: master,
            pty_slave: slave, // keep it open because reasons
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
        panic!(cmd.exec());
    }
}

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3
        || args[1] == "--help"
        || args[1] == "-h"
    {
        eprintln!("usage: {} <rate> <program> [<args>...]", args[0]);
        eprintln!("  run the given program, limiting I/O to the specified number of\
                     bytes per\n
                   second.");
        exit(2);
    }

    let rate: f64 = args[1].parse()
        .unwrap_or_else(|e| {
            eprintln!("error: invalid number for the rate: {}", e);
            exit(2);
        });
    let delay = Delay::from_rate(rate);

    let poll = Poll::new().unwrap();
    let mut events = Events::with_capacity(1024);

    let mut console = unsafe { File::from_raw_fd(0) };
    let ForkResult { child_pid, mut pty_master, pty_slave: _pty_slave } = setup().unwrap();

    let names = ["console", "pty"];

    for (i, mut f) in [&mut console, &mut pty_master].iter_mut().enumerate() {
        set_nonblocking(f).unwrap();
        poll.register(
                &EventedFd(&f.as_raw_fd()),
                Token(i),
                Ready::readable() | UnixReady::error() | UnixReady::hup(),
                PollOpt::level())
            .unwrap();
    }

    'event: loop {
        poll.poll(&mut events, None).unwrap();
        debug!("poll returned");

        for event in &events {
            debug!("{:?}", event);

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
                    break 'event;
                }
                Ok(1) => {
                    debug!("got {:?}", buf[0] as char);

                    dest.write_all(&buf).unwrap();

                    delay.sleep()
                        .unwrap_or_else(|e| {
                            error!("delay error: {}", e);
                        });
                }
                Ok(_) => unreachable!(),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    debug!("wouldblock from {}", names[index]);
                }
                Err(ref e) => {
                    panic!("read error {}", e);
                }
            }
        }
    }

    let mut child_status = 0;
    checkerr(unsafe { libc::waitpid(child_pid, &mut child_status, 0) }, "waitpid")
        .unwrap();

    debug!("resetting tty settings");
    term::reset_tty();

    if child_status != 0 {
        unsafe {
            if libc::WIFEXITED(child_status) {
                let exit_code = libc::WEXITSTATUS(child_status);
                error!("child exited with {}", exit_code);
                std::process::exit(exit_code);
            } else if libc::WIFSIGNALED(child_status) {
                let sig = libc::WTERMSIG(child_status);
                error!("child killed by signal {}", sig);
            } else {
                error!("something happened to the child, status {}", child_status);
            }
            std::process::exit(-1);
        }
    } else {
        debug!("child exited cleanly");
    }

    debug!("returning from main");
}
