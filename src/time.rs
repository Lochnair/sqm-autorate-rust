// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use std::marker::PhantomData;

use rustix::time::{ClockId, clock_gettime};

pub trait Clock {
    const ID: ClockId;
}

pub enum Realtime {}
pub enum Monotonic {}

impl Clock for Realtime {
    const ID: ClockId = ClockId::Realtime;
}

impl Clock for Monotonic {
    const ID: ClockId = ClockId::Monotonic;
}

pub struct ClockSample<C> {
    time_s: u64,
    time_ns: u64,
    _clock: PhantomData<C>,
}

impl<C: Clock> ClockSample<C> {
    pub fn now() -> Self {
        let time = clock_gettime(C::ID);

        Self {
            time_s: time.tv_sec as u64,
            time_ns: time.tv_nsec as u64,
            _clock: PhantomData,
        }
    }

    pub fn secs(&self) -> u64 {
        self.time_s
    }

    pub fn nsecs(&self) -> u64 {
        self.time_ns
    }

    pub fn as_nanos(&self) -> u64 {
        self.time_s * 1_000_000_000 + self.time_ns
    }

    pub fn as_secs_f64(&self) -> f64 {
        self.time_s as f64 + self.time_ns as f64 / 1_000_000_000.0
    }

    pub fn to_milliseconds(&self) -> u64 {
        (self.time_s * 1000) + (self.time_ns / 1000000)
    }
}

impl ClockSample<Realtime> {
    pub fn as_time_since_midnight(&self) -> i64 {
        (self.time_s as i64 % 86_400 * 1000) + self.time_ns as i64 / 1_000_000
    }
}
