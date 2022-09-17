use nix::time::{clock_gettime, ClockId};

pub(crate) struct Utils {}

impl Utils {
    pub(crate) fn a_else_b(a: f64, b: f64) -> f64 {
        return if a > 0.0 { a } else { b };
    }
    pub(crate) fn ewma_factor(tick: f64, dur: f64) -> f64 {
        ((0.5_f64).ln() / (dur / tick)).exp()
    }
    pub(crate) fn get_current_time() -> (f64, f64) {
        let time = clock_gettime(ClockId::CLOCK_MONOTONIC).unwrap();
        (time.tv_sec() as f64, time.tv_nsec() as f64)
    }

    #[cfg(target_endian = "big")]
    pub(crate) fn to_ne(val: u64) -> u64 {
        val.to_be()
    }

    #[cfg(target_endian = "little")]
    pub(crate) fn to_ne(val: u32) -> u32 {
        val.to_le()
    }
}
