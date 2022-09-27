use static_init::dynamic;
use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use log::{Level, LevelFilter, Metadata, Record, SetLoggerError};
use time::format_description::FormatItem;
use time::formatting::Formattable;
use time::macros::format_description;
use time::OffsetDateTime;

const LOG_DATETIME_FORMAT: &[FormatItem] = format_description!(
    "[year]-[month]-[day] [hour]:[minute]:[second] [offset_hour \
         sign:mandatory]:[offset_minute]:[offset_second]"
);

#[derive(Clone, Copy)]
pub struct SimpleLogger {
    pub level: log::Level,
}

fn time_format<T>(dt: T, format: &(impl Formattable + ?Sized)) -> String
where
    T: Into<OffsetDateTime>,
{
    dt.into().format(format).unwrap()
}

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            println!(
                "{} {:5} {}:{}: {}",
                time_format(SystemTime::now(), &LOG_DATETIME_FORMAT),
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
