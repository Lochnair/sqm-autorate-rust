// SPDX-FileCopyrightText: 2022-Present Nils Andreas Svee mailto:contact@lochnair.net (github @Lochnair)
//
// SPDX-License-Identifier: MPL-2.0

use anyhow::anyhow;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub type ArcMutex<T> = Arc<Mutex<T>>;
pub type ArcRwLock<T> = Arc<RwLock<T>>;

pub trait MutexExt<T> {
    fn lock_anyhow(&self) -> anyhow::Result<MutexGuard<'_, T>>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_anyhow(&self) -> anyhow::Result<MutexGuard<'_, T>> {
        self.lock().map_err(|e| anyhow!("mutex poisoned: {e}"))
    }
}

pub trait RwLockExt<T> {
    fn read_anyhow(&self) -> anyhow::Result<RwLockReadGuard<'_, T>>;
    fn write_anyhow(&self) -> anyhow::Result<RwLockWriteGuard<'_, T>>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn read_anyhow(&self) -> anyhow::Result<RwLockReadGuard<'_, T>> {
        self.read().map_err(|e| anyhow!("rwlock poisoned: {e}"))
    }

    fn write_anyhow(&self) -> anyhow::Result<RwLockWriteGuard<'_, T>> {
        self.write().map_err(|e| anyhow!("rwlock poisoned: {e}"))
    }
}
