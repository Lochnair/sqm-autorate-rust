use rustix::fs::Timespec;
use rustix::thread::ClockId;
use rustix::time::clock_gettime;

pub struct Time {
    time_s: u64,
    time_ns: u64,
}

impl Time {
    pub fn new(id: ClockId) -> Self {
        let time: Timespec = clock_gettime(id);
        Self {
            time_s: time.tv_sec as u64,
            time_ns: time.tv_nsec as u64,
        }
    }

    pub fn get_time_since_midnight(&self) -> i64 {
        (self.time_s as i64 % 86400 * 1000) + (self.time_ns as i64 / 1000000)
    }

    pub fn to_milliseconds(&self) -> u64 {
        (self.time_s * 1000) + (self.time_ns / 1000000)
    }
}
