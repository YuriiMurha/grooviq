//! Offline rendering: the real engine, driven faster than real time. Every engine test and the
//! golden renders use it as the reference.

use crate::controller::SendError;
use crate::{BLOCK, Command, EngineConfig, engine};

/// Renders `frames` frames of interleaved stereo, calling the processor with `callback` frames
/// at a time (0 counts as 1) as an audio device would.
///
/// `events` are `(frame, command)`, in any order (events at the same frame keep theirs). They
/// pass the same validation and clamping as [`crate::Controller::send`]; rejected ones are
/// skipped. A note starts at its exact frame; every other command applies at the start of the
/// 64-frame block that contains its frame. So the output does not depend on `callback`.
pub fn render_offline(
    config: EngineConfig,
    events: &[(u64, Command)],
    frames: usize,
    callback: usize,
) -> Vec<f32> {
    let (mut controller, mut processor) = engine(config);
    let mut order: Vec<&(u64, Command)> = events.iter().collect();
    order.sort_by_key(|(frame, _)| *frame);
    let mut waiting = order.into_iter().peekable();
    let mut out = vec![0.0; frames * 2];
    let callback = callback.max(1);
    let mut pos = 0;
    while pos < frames {
        let chunk_end = (pos + callback).min(frames);
        while pos < chunk_end {
            // Queue every event that fits; the processor holds each back until its block.
            while let Some(&&(frame, cmd)) = waiting.peek() {
                if controller.send_at(frame, cmd) == Err(SendError::Full) {
                    break;
                }
                waiting.next();
            }
            // While events wait for room, stop before the block of the first one. If that
            // block is due now, render it anyway (its late events slip to the next block):
            // more events than the queue holds fall inside it.
            let end = match waiting.peek() {
                Some(&&(frame, _)) => {
                    let next_block = usize::try_from(frame).unwrap_or(usize::MAX) / BLOCK * BLOCK;
                    chunk_end.min(next_block.max((pos / BLOCK + 1) * BLOCK))
                }
                None => chunk_end,
            };
            processor.process(&mut out[pos * 2..end * 2], 2);
            pos = end;
        }
    }
    out
}
