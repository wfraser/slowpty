use anyhow::{Context, Result};
use mio::{Events, Poll, Interest, Token};
use mio::unix::SourceFd;
use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

pub struct ReadableSet<'a> {
    mio_poll: Poll,
    console: &'a mut File,
    pty_master: &'a mut File,
    bits: u8,
}

pub enum PollResult {
    /// You're good to go.
    Ok,

    /// At least one of the endpoints is closed or permanently unreadable.
    Closed,
}

pub struct PollEndpoint<'a> {
    pub name: &'static str,
    pub src: &'a mut File,
    pub dst: &'a mut File,
}

fn set_nonblocking(f: &File) -> Result<()> {
    let fd = f.as_raw_fd();
    let previous = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if previous < 0 {
        return Err(io::Error::last_os_error())
            .context("fcntl(F_GETFL)");
    }
    let new = previous | libc::O_NONBLOCK;
    if unsafe { libc::fcntl(fd, libc::F_SETFL, new) } < 0 {
        return Err(io::Error::last_os_error())
            .context("fcntl(F_SETFL)");
    }
    Ok(())
}

impl<'a> ReadableSet<'a> {
    pub fn new(console: &'a mut File, pty_master: &'a mut File) -> Result<Self> {
        let mio_poll = Poll::new().context("mio poll instantiation")?;
        for (i, f) in [&console, &pty_master].iter_mut().enumerate() {
            set_nonblocking(f)
                .with_context(|| format!("failed to set {} nonblocking", Self::name(i)))?;
            mio_poll.registry()
                .register(
                    &mut SourceFd(&f.as_raw_fd()),
                    Token(i),
                    Interest::READABLE,
                )
                .with_context(|| format!("mio poll registration for {}", Self::name(i)))?;
        }
        
        Ok(Self {
            mio_poll,
            console,
            pty_master,
            bits: 0,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }

    fn name(idx: usize) -> &'static str {
        match idx {
            0 => "console",
            1 => "pty",
            _ => panic!(),
        }
    }

    pub fn block(&mut self) -> Result<PollResult> {
        debug!("mio poll");
        let mut events = Events::with_capacity(2);
        self.mio_poll.poll(&mut events, None).context("mio poll")?;

        for event in events.into_iter() {
            debug!("{:?}", event);
            let index = event.token().0 as usize;

            if event.is_read_closed() && !event.is_readable() {
                // Don't even try to read in this state. Even with O_NONBLOCK set, it may still
                // block.
                debug!("endpoint closed: {}", Self::name(index));
                return Ok(PollResult::Closed);
            }

            self.bits |= (1 << index) as u8;
        }

        Ok(PollResult::Ok)
    }

    pub fn endpoint(&mut self, idx: usize) -> Option<PollEndpoint> {
        match idx {
            0 => Some(PollEndpoint {
                name: "console",
                src: self.console,
                dst: self.pty_master,
            }),
            1 => Some(PollEndpoint {
                name: "pty",
                src: self.pty_master,
                dst: self.console,
            }),
            _ => None,
        }
    }

    pub fn unset(&mut self, index: usize) {
        let mask = (1 << index) as u8;
        self.bits &= !mask;
    }
}
