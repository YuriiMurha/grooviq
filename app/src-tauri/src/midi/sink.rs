//! The engine's side of a keyboard: commands go to the controller as they come.

use super::CommandSink;
use crate::lock;
use grooviq_engine::{ChannelId, Command, Controller};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// The most releases kept for later: far above the keys a person holds.
const MAX_OWED: usize = 1024;

/// Sends to the engine's controller (its lock only, never the session's).
///
/// A note-off the engine refuses because its command queue is full would leave the note ringing
/// forever, so it is kept and tried again (after every message and by the poll) until the engine
/// takes it. One refused because its channel is gone is dropped: nothing is left to ring.
///
/// Lock order: the owed set, then the controller.
pub struct ControllerSink {
    controller: Arc<Mutex<Controller>>,
    /// Releases the engine owes: (channel, key).
    owed: Mutex<BTreeSet<(u32, u8)>>,
}

impl ControllerSink {
    pub fn new(controller: Arc<Mutex<Controller>>) -> Self {
        Self {
            controller,
            owed: Mutex::new(BTreeSet::new()),
        }
    }

    /// Note-offs the engine refused and still owes.
    pub fn owed(&self) -> usize {
        lock(&self.owed).len()
    }
}

fn exists(controller: &Controller, channel: ChannelId) -> bool {
    controller.state().channels.iter().any(|c| c.id == channel)
}

impl CommandSink for ControllerSink {
    fn send(&self, cmd: Command) {
        let mut owed = lock(&self.owed);
        let mut controller = lock(&self.controller);
        match cmd {
            Command::ChannelNote {
                channel,
                key,
                on: false,
                ..
            } => {
                if controller.send(cmd).is_ok() {
                    owed.remove(&(channel.0, key));
                } else if exists(&controller, channel) && owed.len() < MAX_OWED {
                    owed.insert((channel.0, key));
                }
            }
            Command::ChannelNote {
                channel,
                key,
                on: true,
                ..
            } => {
                // A release still owed would land after this note and cut it.
                owed.remove(&(channel.0, key));
                let _ = controller.send(cmd);
            }
            other => {
                let _ = controller.send(other);
            }
        }
    }

    fn retry(&self) {
        let mut owed = lock(&self.owed);
        if owed.is_empty() {
            return;
        }
        let mut controller = lock(&self.controller);
        owed.retain(|&(channel, key)| {
            let channel = ChannelId(channel);
            let off = Command::ChannelNote {
                channel,
                key,
                velocity: 0.0,
                on: false,
            };
            exists(&controller, channel) && controller.send(off).is_err()
        });
    }
}
