use libc;

use std::fs::File;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd};

use ::checkerr;

pub struct PtyPair {
    pub master: File,
    pub slave: File,
}

pub fn open_pty_pair() -> io::Result<PtyPair> {
    let master = unsafe {
        File::from_raw_fd(checkerr(libc::posix_openpt(libc::O_RDWR), "posix_openpt")?)
    };

    checkerr(unsafe { libc::grantpt(master.as_raw_fd()) }, "grantpt")?;
    checkerr(unsafe { libc::unlockpt(master.as_raw_fd()) }, "unlockpt")?;

    let slavename: *const libc::c_char = unsafe { libc::ptsname(master.as_raw_fd()) };
    if slavename.is_null() {
        let e = io::Error::last_os_error();
        eprintln!("ptsname: {}", e);
        return Err(e);
    }

    let slave = unsafe {
        File::from_raw_fd(checkerr(libc::open(slavename, libc::O_RDWR), "open slave")?)
    };

    #[cfg(target_os = "linux")]
    {
        checkerr(unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETPIPE_SZ, 1) },
            "F_SETPIPE_SZ, master")?;
        checkerr(unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_SETPIPE_SZ, 1) },
            "F_SETPIPE_SZ, slave")?;
    }

    Ok(PtyPair { master, slave })
}
