//! Frequency results shared by oscillator tracks within one render span.

use super::{Bufs, MAX_BLOCK, SLOTS, Voice, pitch_hz};

const ROWS: usize = 8;
const LOOKUP_SLOTS: usize = 128;
const NO_ROW: u8 = u8::MAX;

#[derive(Clone, Copy)]
struct Candidate {
    band: u32,
    track: u8,
}

pub(super) struct Plan {
    pub(super) tracks: [[u8; 3]; SLOTS],
    pub(super) rows: usize,
}

impl Plan {
    pub(super) fn new(voices: &[Voice; SLOTS], b: &Bufs, start: usize) -> Self {
        let mut plan = Self {
            tracks: [[NO_ROW; 3]; SLOTS],
            rows: 0,
        };
        let mut lookup = [None::<Candidate>; LOOKUP_SLOTS];
        for (slot, voice) in voices.iter().enumerate() {
            if !voice.active {
                continue;
            }
            let pm = voice.pitch + b.pitch[start];
            for (osc, p) in [pm + b.off1[start], pm + b.off2[start], pm - 12.0]
                .into_iter()
                .enumerate()
            {
                // Each track renders a whole span at once. Reserve a row only when
                // two tracks may share it. Group nearby starting pitches to tolerate
                // small rounding differences as the LFO moves; hits still need all bits.
                let band = p.to_bits().wrapping_add(32) >> 6;
                let index = (band.wrapping_mul(0x9E37_79B9) >> 25) as usize;
                let peer = &mut lookup[index];
                if let Some(peer) = peer.filter(|peer| peer.band == band) {
                    let peer_track = usize::from(peer.track);
                    let source = &mut plan.tracks[peer_track / 3][peer_track % 3];
                    if *source == NO_ROW && plan.rows < ROWS {
                        *source = plan.rows as u8;
                        plan.rows += 1;
                    }
                    plan.tracks[slot][osc] = *source;
                } else {
                    *peer = Some(Candidate {
                        band,
                        track: (3 * slot + osc) as u8,
                    });
                }
            }
        }
        plan
    }
}

#[derive(Clone, Copy)]
struct Frequency {
    pitch: u32,
    hz: f32,
}

pub(super) struct Cache {
    rows: [[Option<Frequency>; MAX_BLOCK]; ROWS],
}

impl Cache {
    pub(super) fn new() -> Self {
        Self {
            rows: [[None; MAX_BLOCK]; ROWS],
        }
    }

    pub(super) fn clear(&mut self, rows: usize, frames: usize) {
        for row in &mut self.rows[..rows] {
            row[..frames].fill(None);
        }
    }

    #[inline(always)]
    pub(super) fn hz(&mut self, row: u8, frame: usize, p: f32) -> f32 {
        let Some(row) = self.rows.get_mut(usize::from(row)) else {
            return pitch_hz(p);
        };
        let cell = &mut row[frame];
        let pitch = p.to_bits();
        if let Some(frequency) = cell
            && frequency.pitch == pitch
        {
            return frequency.hz;
        }
        let hz = pitch_hz(p);
        *cell = Some(Frequency { pitch, hz });
        hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_uses_all_pitch_bits() {
        let mut cache = Cache::new();
        let p = 60.0_f32;
        let adjacent = f32::from_bits(p.to_bits() + 1);
        assert_ne!(pitch_hz(p).to_bits(), pitch_hz(adjacent).to_bits());
        assert_eq!(cache.hz(0, 0, p).to_bits(), pitch_hz(p).to_bits());
        assert_eq!(
            cache.hz(0, 0, adjacent).to_bits(),
            pitch_hz(adjacent).to_bits()
        );
    }

    #[test]
    fn entries_belong_to_one_sample_frame_and_span() {
        let mut cache = Cache::new();
        cache.hz(0, 0, 60.0);
        assert!(cache.rows[0][0].is_some());
        assert!(cache.rows[0][1].is_none());
        cache.hz(0, 1, 60.0);
        assert!(cache.rows[0][1].is_some());
        cache.clear(1, 2);
        assert!(cache.rows[0][..2].iter().all(Option::is_none));
    }
}
