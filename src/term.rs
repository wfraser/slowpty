use libc;
use std::io;
use std::mem;
use std::process;
use std::os::unix::io::RawFd;

use ::checkerr;

static mut ORIGINAL_TERM_SETTINGS:
    [u8; mem::size_of::<libc::termios>()] =
    [0u8; mem::size_of::<libc::termios>()];

unsafe fn original_term_settings() -> *mut libc::termios {
    &mut ORIGINAL_TERM_SETTINGS as *mut [u8] as *mut libc::termios
}

extern "C" fn reset_tty() {
    unsafe {
        // note: can't print anything here
        let result = libc::tcsetattr(0, libc::TCSANOW, original_term_settings());
        if 1 == result {
            libc::abort()
        }
    }
}

pub fn set_raw(fd: RawFd) -> io::Result<()> {
    let mut t = unsafe { *original_term_settings() }.clone();
    unsafe { libc::cfmakeraw(&mut t as *mut _) };
    checkerr(unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &t as *const _) },
        "tcsetattr(raw)")?;
    Ok(())
}

pub fn save_term_settings(fd: RawFd) -> io::Result<()> {
    let mut ws = unsafe { mem::zeroed::<libc::winsize>() };
    checkerr(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut _) },
        "ioctl(TIOGCWINSZ)")?;

    /*
    unsafe {
        ORIGINAL_TERM_SETTINGS = Box::into_raw(Box::new(mem::zeroed()));
    }
    */

    checkerr(unsafe { libc::tcgetattr(fd, original_term_settings()) },
        "tcgetattr(original settings")?;

    Ok(())
}

pub fn restore_term_settings_at_exit() -> io::Result<()> {
    checkerr(unsafe { libc::atexit(reset_tty) }, "atexit")?;
    Ok(())
}

pub fn set_controlling_tty(fd: RawFd) -> io::Result<()> {
    checkerr(unsafe { libc::ioctl(fd, libc::TIOCSCTTY as u64, 1) }, "ioctl(TIOCSCTTY)")
        .map(|_| ())
}

pub fn set_session_leader() -> io::Result<()> {
    checkerr(unsafe { libc::setsid() }, "setsid")?;
    Ok(())
}

pub struct WindowSize {
    ws: libc::winsize,
}

impl WindowSize {
    pub fn get(fd: RawFd) -> io::Result<Self> {
        let mut ws = unsafe { mem::zeroed::<libc::winsize>() };
        checkerr(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut _) },
            "ioctl(TIOCGWINSZ)")?;
        Ok(WindowSize { ws })
    }

    pub fn set(&self, fd: RawFd) -> io::Result<()> {
        checkerr(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &self.ws) },
            "ioctl(TIOCSWINSZ)")?;
        Ok(())
    }
}
