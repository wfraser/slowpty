use libc;
use std;
use std::io;

pub const SEC_NS: i64 = 1_000_000_000;

pub struct Delay {
    ts: libc::timespec,
}

impl Delay {
    pub fn from_rate(rate: f64) -> Self {
        let delay_nanos = (SEC_NS as f64 / rate) as i64;
        Delay::from_nanos(delay_nanos)
    }

    pub fn from_nanos(nanos: i64) -> Self {
        Delay {
            ts: libc::timespec {
                tv_sec: nanos / SEC_NS,
                tv_nsec: nanos % SEC_NS,
            },
        }
    }

    pub fn sleep(&self) -> io::Result<()> {
        let mut delay = self.ts;
        loop {
            let mut remaining = unsafe { std::mem::zeroed::<libc::timespec>() };
            match unsafe { libc::nanosleep(&delay, &mut remaining) } {
                0 => return Ok(()),
                _ => {
                    let e = io::Error::last_os_error();
                    if e.kind() == io::ErrorKind::Interrupted {
                        delay.tv_sec = remaining.tv_sec;
                        delay.tv_nsec = remaining.tv_nsec;
                    } else {
                        eprintln!("nanosleep: {}", e);
                        return Err(e);
                    }
                }
            }
        }
    }
}
