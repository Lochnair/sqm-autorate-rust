use log::{Level, LevelFilter, Metadata, Record, SetLoggerError};
use rustix::thread::ClockId;

use crate::time::Time;

#[derive(Clone, Copy)]
pub struct SimpleLogger {
    pub level: Level,
}

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let now = Time::new(ClockId::Realtime);
            println!(
                "{} {:5} {}:{}: {}",
                now.as_secs_f64(),
                record.level(),
                record.file().unwrap(),
                record.line().unwrap(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

pub fn init(level: Level) -> Result<(), SetLoggerError> {
    log::set_boxed_logger(Box::new(SimpleLogger { level }))
        .map(|()| log::set_max_level(LevelFilter::Trace))
}
