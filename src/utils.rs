use nix::errno::Errno;
use nix::time::{clock_gettime, ClockId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum UtilError {
    #[error("libc error")]
    Libc(#[from] Errno),
}

pub struct Utils {}

impl Utils {
    pub fn a_else_b(a: f64, b: f64) -> f64 {
        return if a > 0.0 { a } else { b };
    }
    pub fn ewma_factor(tick: f64, dur: f64) -> f64 {
        ((0.5_f64).ln() / (dur / tick)).exp()
    }
    pub fn get_current_time() -> Result<(f64, f64), UtilError> {
        let time = clock_gettime(ClockId::CLOCK_MONOTONIC)?;
        Ok((time.tv_sec() as f64, time.tv_nsec() as f64))
    }

    #[cfg(target_endian = "big")]
    pub fn to_ne(val: u64) -> u64 {
        val.to_be()
    }

    #[cfg(target_endian = "little")]
    pub fn to_ne(val: u32) -> u32 {
        val.to_le()
    }
}
