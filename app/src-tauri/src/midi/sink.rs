//! The engine's side of a keyboard: commands go to the controller as they come.

use super::CommandSink;
use crate::lock;
use grooviq_engine::{ChannelId, Command, Controller};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Releases the engine owes on one channel: a bit per key.
struct Owed {
    /// The channel instance the keys sound on (`Controller::channel_instance`).
    instance: u64,
    keys: u128,
}

/// Sends to the engine's controller (its lock only, never the session's).
///
/// A note-off the engine refuses because its command queue is full would leave the note ringing
/// forever, so it is kept and tried again until the engine takes it. The debt is a bit per key
/// per channel that exists, so it cannot overflow and no release is ever forgotten. It belongs
/// to the channel instance that was sounding: one refused because its channel is gone, or
/// outlived by another channel that took the same id, is dropped, as the engine already cut
/// those notes.
///
/// Lock order: the owed set, then the controller.
pub struct ControllerSink {
    controller: Arc<Mutex<Controller>>,
    /// Releases the engine owes, by channel.
    owed: Mutex<BTreeMap<u32, Owed>>,
}

impl ControllerSink {
    pub fn new(controller: Arc<Mutex<Controller>>) -> Self {
        Self {
            controller,
            owed: Mutex::new(BTreeMap::new()),
        }
    }

    /// Note-offs the engine refused and still owes.
    pub fn owed(&self) -> usize {
        lock(&self.owed)
            .values()
            .map(|o| o.keys.count_ones() as usize)
            .sum()
    }
}

fn off(channel: u32, key: u8) -> Command {
    Command::ChannelNote {
        channel: ChannelId(channel),
        key,
        velocity: 0.0,
        on: false,
    }
}

impl CommandSink for ControllerSink {
    fn press(&self, channel: u32, key: u8, velocity: f32) -> Option<u64> {
        let mut owed = lock(&self.owed);
        let mut controller = lock(&self.controller);
        let id = ChannelId(channel);
        let instance = controller.channel_instance(id)?;
        // A release still owed would land after this note and cut it.
        if let Some(debt) = owed.get_mut(&channel) {
            debt.keys &= !(1u128 << key);
        }
        let note = Command::ChannelNote {
            channel: id,
            key,
            velocity,
            on: true,
        };
        let sent = controller.send(note).is_ok();
        owed.retain(|_, debt| debt.keys != 0);
        sent.then_some(instance)
    }

    fn release(&self, channel: u32, key: u8, instance: u64) {
        let mut owed = lock(&self.owed);
        let mut controller = lock(&self.controller);
        // Checked under the controller's lock: the channel is still the one the key sounded on.
        if controller.channel_instance(ChannelId(channel)) != Some(instance) {
            return;
        }
        if controller.send(off(channel, key)).is_ok() {
            if let Some(debt) = owed.get_mut(&channel) {
                debt.keys &= !(1u128 << key);
            }
        } else {
            // Refused with the channel still there: the queue is full.
            let debt = owed.entry(channel).or_insert(Owed { instance, keys: 0 });
            if debt.instance != instance {
                // What was owed belonged to a channel this id no longer is.
                *debt = Owed { instance, keys: 0 };
            }
            debt.keys |= 1u128 << key;
        }
        owed.retain(|_, debt| debt.keys != 0);
    }

    fn retry(&self) {
        let mut owed = lock(&self.owed);
        if owed.is_empty() {
            return;
        }
        let mut controller = lock(&self.controller);
        owed.retain(|&id, debt| {
            let channel = ChannelId(id);
            // The channel it was owed to is gone or replaced: nothing is left to ring.
            if controller.channel_instance(channel) != Some(debt.instance) {
                return false;
            }
            for key in 0..128u8 {
                if debt.keys & (1u128 << key) != 0 && controller.send(off(id, key)).is_ok() {
                    debt.keys &= !(1u128 << key);
                }
            }
            debt.keys != 0
        });
    }
}
