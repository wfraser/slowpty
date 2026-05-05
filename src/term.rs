use anyhow::Result;
use std::mem;
use std::os::unix::io::RawFd;

use crate::checkerr;

pub fn set_controlling_tty(fd: RawFd) -> Result<()> {
    #[allow(clippy::useless_conversion)] // it isn't identical on all platforms
    checkerr(unsafe { libc::ioctl(fd, libc::TIOCSCTTY.into(), 1) }, "ioctl(TIOCSCTTY)")
        .map(|_| ())
}

pub fn set_session_leader() -> Result<()> {
    checkerr(unsafe { libc::setsid() }, "setsid")?;
    Ok(())
}

pub struct WindowSize {
    ws: libc::winsize,
}

impl WindowSize {
    pub fn from_fd(fd: RawFd) -> Result<Self> {
        let mut ws: libc::winsize = unsafe { mem::zeroed() };
        checkerr(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut _) },
            "ioctl(TIOCGWINSZ)")?;
        Ok(WindowSize { ws })
    }

    pub fn apply_to_fd(&self, fd: RawFd) -> Result<()> {
        checkerr(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &self.ws) },
            "ioctl(TIOCSWINSZ)")?;
        Ok(())
    }
}

pub struct TermSettings {
    termios: libc::termios,
    fd: RawFd,
}

impl TermSettings {
    /// Get the current terminal settings for the given fd.
    pub fn current(fd: RawFd) -> Result<Self> {
        let mut termios: libc::termios = unsafe { mem::zeroed() };
        checkerr(unsafe { libc::tcgetattr(fd, &mut termios) },
            "tcgetattr(original settings)")?;

        Ok(Self { termios, fd })
    }

    /// Set the terminal to raw mode; the previously saved settings are used as a basis.
    pub fn set_raw(&self) -> Result<()> {
        let mut t = self.termios;
        unsafe { libc::cfmakeraw(&mut t as *mut _) };
        checkerr(unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &t as *const _) },
            "tcsetattr(raw)")?;
        Ok(())
    }

    /// Reset the terminal back to original saved settings.
    pub fn reset(mut self) -> Result<()> {
        self.internal_reset()
    }

    fn internal_reset(&mut self) -> Result<()> {
        checkerr(unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.termios)},
            "tcsetattr(original settings)")?;
        Ok(())
    }
}

impl Drop for TermSettings {
    fn drop(&mut self) {
        if self.internal_reset().is_err() {
            // note: don't print anything here since this is likely to run on exit
            unsafe { libc::abort() }
        }
    }
}
