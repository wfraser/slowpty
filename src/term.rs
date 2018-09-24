use libc;
use std::io;
use std::mem;
use std::os::unix::io::RawFd;

use ::checkerr;

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

pub fn set_raw(fd: RawFd) -> io::Result<()> {
    let mut t = unsafe { ORIGINAL_TERM_SETTINGS }
        .expect("original terminal settings not set yet!");
    unsafe { libc::cfmakeraw(&mut t as *mut _) };
    checkerr(unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &t as *const _) },
        "tcsetattr(raw)")?;
    Ok(())
}

pub fn save_term_settings(fd: RawFd) -> io::Result<()> {
    let mut ws = unsafe { mem::zeroed::<libc::winsize>() };
    checkerr(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut _) },
        "ioctl(TIOGCWINSZ)")?;

    let mut settings: libc::termios = unsafe { mem::zeroed() };
    checkerr(unsafe { libc::tcgetattr(fd, &mut settings) },
        "tcgetattr(original settings")?;

    unsafe { ORIGINAL_TERM_SETTINGS = Some(settings); }

    Ok(())
}

pub fn restore_term_settings_at_exit() -> io::Result<()> {
    checkerr(unsafe { libc::atexit(reset_tty) }, "atexit")?;
    Ok(())
}

pub fn set_controlling_tty(fd: RawFd) -> io::Result<()> {
    checkerr(unsafe { libc::ioctl(fd, u64::from(libc::TIOCSCTTY), 1) }, "ioctl(TIOCSCTTY)")
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
    pub fn get_from_fd(fd: RawFd) -> io::Result<Self> {
        let mut ws = unsafe { mem::zeroed::<libc::winsize>() };
        checkerr(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut _) },
            "ioctl(TIOCGWINSZ)")?;
        Ok(WindowSize { ws })
    }

    pub fn apply_to_fd(&self, fd: RawFd) -> io::Result<()> {
        checkerr(unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &self.ws) },
            "ioctl(TIOCSWINSZ)")?;
        Ok(())
    }
}
