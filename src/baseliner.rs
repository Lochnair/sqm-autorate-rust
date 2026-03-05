use crate::component::Baseliner;
use crate::pinger::PingReply;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, RwLock};

/// Thin thread wrapper: receives PingReply from the pinger, dispatches to the
/// Baseliner implementation (write-locked), and forwards reselection signals.
pub fn run(
    baseliner: Arc<RwLock<dyn Baseliner>>,
    rx: Receiver<PingReply>,
    reselect_tx: Sender<bool>,
) -> anyhow::Result<()> {
    loop {
        let ping = rx.recv()?;
        let trigger = baseliner
            .write()
            .expect("baseliner RwLock poisoned")
            .on_ping(ping);
        if trigger {
            let _ = reselect_tx.send(true);
        }
    }
}
