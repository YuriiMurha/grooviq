//! The engine's side of a keyboard: commands go to the controller as they come.

use super::CommandSink;
use crate::lock;
use grooviq_engine::{ChannelId, Command, Controller};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// Releases the engine owes on one channel: how many, per key.
struct Owed {
    /// The channel instance the keys sound on (`Controller::channel_instance`).
    instance: u64,
    keys: [u32; 128],
}

impl Owed {
    fn new(instance: u64) -> Self {
        Self {
            instance,
            keys: [0; 128],
        }
    }

    fn is_empty(&self) -> bool {
        self.keys.iter().all(|&n| n == 0)
    }

    /// Sends as many of the key's owed releases as the engine takes.
    fn pay(&mut self, controller: &mut Controller, channel: u32, key: u8) {
        let owed = &mut self.keys[usize::from(key)];
        while *owed > 0 && controller.send(off(channel, key)).is_ok() {
            *owed -= 1;
        }
    }
}

/// Sends to the engine's controller (its lock only, never the session's).
///
/// The instruments pair the n-th release of a key with its n-th press, so a release that cannot
/// be sent now is owed, not cancelled by a later press: the late release ends the older note and
/// the newer one sounds on. A release the engine refuses because its command queue is full is
/// kept as a count per key per channel that exists, so it cannot overflow and none is ever
/// forgotten, and is sent again until the engine takes it. A key's older debt is paid before a
/// newer release is sent. The debt belongs to the channel instance that was sounding: one
/// refused because its channel is gone, or outlived by another channel that took the same id,
/// is dropped, as the engine already cut those notes.
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

    /// Releases the engine refused and still owes.
    pub fn owed(&self) -> usize {
        lock(&self.owed)
            .values()
            .flat_map(|o| o.keys.iter())
            .map(|&n| n as usize)
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
        let _owed = lock(&self.owed);
        let mut controller = lock(&self.controller);
        let id = ChannelId(channel);
        let instance = controller.channel_instance(id)?;
        let note = Command::ChannelNote {
            channel: id,
            key,
            velocity,
            on: true,
        };
        controller.send(note).is_ok().then_some(instance)
    }

    fn release(&self, channel: u32, key: u8, instance: u64, count: u32) {
        let mut owed = lock(&self.owed);
        let mut controller = lock(&self.controller);
        // Checked under the controller's lock: the channel is still the one the key sounded on.
        if controller.channel_instance(ChannelId(channel)) != Some(instance) {
            return;
        }
        let debt = owed.entry(channel).or_insert_with(|| Owed::new(instance));
        if debt.instance != instance {
            // What was owed belonged to a channel this id no longer is.
            *debt = Owed::new(instance);
        }
        let slot = &mut debt.keys[usize::from(key)];
        *slot = slot.saturating_add(count);
        debt.pay(&mut controller, channel, key);
        owed.retain(|_, debt| !debt.is_empty());
    }

    fn retry(&self) {
        let mut owed = lock(&self.owed);
        if owed.is_empty() {
            return;
        }
        let mut controller = lock(&self.controller);
        owed.retain(|&channel, debt| {
            // The channel it was owed to is gone or replaced: nothing is left to ring.
            if controller.channel_instance(ChannelId(channel)) != Some(debt.instance) {
                return false;
            }
            for key in 0..128u8 {
                debt.pay(&mut controller, channel, key);
            }
            !debt.is_empty()
        });
    }
}
