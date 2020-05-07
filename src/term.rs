use anyhow::Result;
use std::io;
use std::mem;
use std::os::unix::io::RawFd;

use crate::checkerr;

static mut ORIGINAL_TERM_SETTINGS: Option<libc::termios> = None;

pub extern "C" fn reset_tty() {
    unsafe {
        // note: can't print anything here
        if let Some(settings) = ORIGINAL_TERM_SETTINGS.take() {
            let result = libc::tcsetattr(0, libc::TCSANOW, &settings);
            let _e = io::Error::last_os_error();
            if -1 == result {
                libc::abort()
            }
        }
    }
}

pub fn set_raw(fd: RawFd) -> Result<()> {
    let mut t = unsafe { ORIGINAL_TERM_SETTINGS }
        .ok_or_else(|| anyhow!("original terminal settings not set yet!"))?;
    unsafe { libc::cfmakeraw(&mut t as *mut _) };
    checkerr(unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &t as *const _) },
        "tcsetattr(raw)")?;
    Ok(())
}

pub fn save_term_settings(fd: RawFd) -> Result<()> {
    let mut settings: libc::termios = unsafe { mem::zeroed() };
    checkerr(unsafe { libc::tcgetattr(fd, &mut settings) },
        "tcgetattr(original settings)")?;

    unsafe { ORIGINAL_TERM_SETTINGS = Some(settings); }

    Ok(())
}

pub fn restore_term_settings_at_exit() -> Result<()> {
    checkerr(unsafe { libc::atexit(reset_tty) }, "atexit")?;
    Ok(())
}

pub fn set_controlling_tty(fd: RawFd) -> Result<()> {
    #[allow(clippy::identity_conversion)] // it isn't identical on all platforms
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
