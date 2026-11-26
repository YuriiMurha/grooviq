//! One visible-track lease. The controller owns request writes; the processor owns result
//! writes, including invalidation. All payload fields are atomic, so a racing read can make
//! one attempt and discard the snapshot if a write overlapped it.

use crate::BLOCK;
use crate::mixer::{MASTER, MixerTrackId};
use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

/// Maximum number of visible tracks in a single meter lease.
pub const MIXER_METER_MAX_TRACKS: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct MixerMeter {
    /// The revision installed on the audio thread when this block rendered.
    pub mix_revision: u64,
    /// End of the captured block on the engine's sample clock.
    pub frames: u64,
    /// In track ID order. Master has its own meter in the live stream.
    pub tracks: Vec<MixerTrackMeter>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MixerTrackMeter {
    pub track: MixerTrackId,
    /// Peak magnitude of each guarded post-fader channel over this block.
    pub peak: [f32; 2],
    /// End frame of the latest block that reached full scale on either channel in this lease.
    pub clip_frame: Option<u64>,
}

#[derive(Clone, Copy)]
pub(crate) struct MeterGraph {
    pub(crate) revision: u64,
    pub(crate) epoch: u64,
}

#[derive(Debug, Default)]
pub(crate) struct MixerMeterShared {
    request: Request,
    result: ResultSlot,
}

#[derive(Debug)]
struct Request {
    stamp: AtomicU64,
    until: AtomicU64,
    revision: AtomicU64,
    epoch: AtomicU64,
    count: AtomicU64,
    ids: [AtomicU64; MIXER_METER_MAX_TRACKS],
}

impl Default for Request {
    fn default() -> Self {
        Self {
            stamp: AtomicU64::new(0),
            until: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            epoch: AtomicU64::new(0),
            count: AtomicU64::new(0),
            ids: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl MixerMeterShared {
    pub(crate) fn poll(
        &self,
        graph: MeterGraph,
        clock: u64,
        lease_frames: u64,
        tracks: &[MixerTrackId],
    ) -> Option<MixerMeter> {
        let stamp = self.request.renew(graph, clock, lease_frames, tracks);
        if tracks.is_empty() {
            return None;
        }
        self.result.read(stamp, graph.revision)
    }
}

impl Request {
    fn matches(&self, graph: MeterGraph) -> bool {
        self.revision.load(SeqCst) == graph.revision && self.epoch.load(SeqCst) == graph.epoch
    }

    fn renew(
        &self,
        graph: MeterGraph,
        clock: u64,
        lease_frames: u64,
        tracks: &[MixerTrackId],
    ) -> u64 {
        let until = if tracks.is_empty() {
            0
        } else {
            clock.saturating_add(lease_frames)
        };
        let stamp = self.stamp.load(SeqCst);
        let count = self.count.load(SeqCst) as usize;
        if tracks.is_empty() && count == 0 {
            return stamp;
        }
        // The controller is serialized. A renewal updates only the deadline, keeping the
        // selection stamp valid for captures and clip events since the previous display poll.
        if count == tracks.len()
            && clock < self.until.load(SeqCst)
            && self.matches(graph)
            && self
                .ids
                .iter()
                .zip(tracks)
                .all(|(a, b)| a.load(SeqCst) == b.0)
        {
            self.until.store(until, SeqCst);
            return stamp;
        }

        let odd = stamp.wrapping_add(1);
        self.stamp.store(odd, SeqCst);
        self.until.store(until, SeqCst);
        self.revision.store(graph.revision, SeqCst);
        self.epoch.store(graph.epoch, SeqCst);
        for (dst, id) in self.ids.iter().zip(tracks) {
            dst.store(id.0, SeqCst);
        }
        self.count.store(tracks.len() as u64, SeqCst);
        let even = odd.wrapping_add(1);
        self.stamp.store(even, SeqCst);
        even
    }

    fn selected(&self, graph: MeterGraph, clock: u64) -> Option<Selection> {
        let stamp = self.stamp.load(SeqCst);
        if stamp == 0 || !stamp.is_multiple_of(2) || clock >= self.until.load(SeqCst) {
            return None;
        }
        let count = self.count.load(SeqCst) as usize;
        if count == 0 || !self.matches(graph) {
            return None;
        }
        let mut ids = [MASTER; MIXER_METER_MAX_TRACKS];
        for (id, src) in ids[..count].iter_mut().zip(&self.ids) {
            *id = MixerTrackId(src.load(SeqCst));
        }
        (self.stamp.load(SeqCst) == stamp).then_some(Selection { stamp, count, ids })
    }
}

struct Selection {
    stamp: u64,
    count: usize,
    ids: [MixerTrackId; MIXER_METER_MAX_TRACKS],
}

#[derive(Debug, Default)]
struct Row {
    id: AtomicU64,
    peak: AtomicU64,
    clip: AtomicU64,
}

#[derive(Debug)]
struct ResultSlot {
    stamp: AtomicU64,
    lease: AtomicU64,
    revision: AtomicU64,
    frames: AtomicU64,
    count: AtomicU64,
    rows: [Row; MIXER_METER_MAX_TRACKS],
}

impl Default for ResultSlot {
    fn default() -> Self {
        Self {
            stamp: AtomicU64::new(0),
            lease: AtomicU64::new(0),
            revision: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            count: AtomicU64::new(0),
            rows: std::array::from_fn(|_| Row::default()),
        }
    }
}

impl ResultSlot {
    fn begin_write(&self) -> u64 {
        let odd = self.stamp.load(SeqCst).wrapping_add(1);
        self.stamp.store(odd, SeqCst);
        odd.wrapping_add(1)
    }

    fn read(&self, lease: u64, revision: u64) -> Option<MixerMeter> {
        let stamp = self.stamp.load(SeqCst);
        if !stamp.is_multiple_of(2)
            || lease == 0
            || self.lease.load(SeqCst) != lease
            || self.revision.load(SeqCst) != revision
        {
            return None;
        }
        let frames = self.frames.load(SeqCst);
        let count = self.count.load(SeqCst) as usize;
        let tracks = self.rows[..count]
            .iter()
            .map(|row| {
                let peak = row.peak.load(SeqCst);
                let clip = row.clip.load(SeqCst);
                MixerTrackMeter {
                    track: MixerTrackId(row.id.load(SeqCst)),
                    peak: [
                        f32::from_bits(peak as u32),
                        f32::from_bits((peak >> 32) as u32),
                    ],
                    clip_frame: (clip != 0).then_some(clip),
                }
            })
            .collect();
        (count != 0 && self.stamp.load(SeqCst) == stamp).then_some(MixerMeter {
            mix_revision: revision,
            frames,
            tracks,
        })
    }
}

/// Callback-owned clip history indexed by the canonical request rows.
pub(crate) struct MixerMeterObserver {
    lease: u64,
    clips: [u64; MIXER_METER_MAX_TRACKS],
}

impl Default for MixerMeterObserver {
    fn default() -> Self {
        Self {
            lease: 0,
            clips: [0; MIXER_METER_MAX_TRACKS],
        }
    }
}

impl MixerMeterObserver {
    pub(crate) fn observe(
        &mut self,
        shared: &MixerMeterShared,
        graph: MeterGraph,
        start: u64,
        mut tap: impl FnMut(MixerTrackId) -> Option<[f32; 2]>,
    ) {
        let Some(selection) = shared.request.selected(graph, start) else {
            return;
        };
        if self.lease != selection.stamp {
            self.clips[..selection.count].fill(0);
            self.lease = selection.stamp;
        }
        let frames = start + BLOCK as u64;
        let result = &shared.result;
        let stamp = result.begin_write();
        let mut count = 0;
        for (i, &id) in selection.ids[..selection.count].iter().enumerate() {
            if let Some(peak) = tap(id) {
                if peak[0] >= 1.0 || peak[1] >= 1.0 {
                    self.clips[i] = frames;
                }
                let row = &result.rows[count];
                row.id.store(id.0, SeqCst);
                row.peak.store(
                    u64::from(peak[0].to_bits()) | (u64::from(peak[1].to_bits()) << 32),
                    SeqCst,
                );
                row.clip.store(self.clips[i], SeqCst);
                count += 1;
            }
        }
        result.lease.store(selection.stamp, SeqCst);
        result.revision.store(graph.revision, SeqCst);
        result.frames.store(frames, SeqCst);
        result.count.store(count as u64, SeqCst);
        result.stamp.store(stamp, SeqCst);
    }

    pub(crate) fn clear(&mut self, shared: &MixerMeterShared) {
        if self.lease != 0 {
            self.lease = 0;
            let stamp = shared.result.begin_write();
            shared.result.lease.store(0, SeqCst);
            shared.result.stamp.store(stamp, SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPH: MeterGraph = MeterGraph {
        revision: 1,
        epoch: 2,
    };

    #[test]
    fn mixer_meter_attempts_only_leased_taps_and_skips_stale_or_contended_requests() {
        let shared = MixerMeterShared::default();
        let mut meter = MixerMeterObserver::default();
        let tracks =
            std::array::from_fn::<_, MIXER_METER_MAX_TRACKS, _>(|i| MixerTrackId(i as u64 + 1));
        let check = |meter: &mut MixerMeterObserver, graph, start, expected_calls| {
            let mut calls = 0;
            meter.observe(&shared, graph, start, |track| {
                assert_eq!(track, tracks[calls]);
                calls += 1;
                Some([0.0; 2])
            });
            assert_eq!(calls, expected_calls);
        };

        check(&mut meter, GRAPH, 0, 0);
        assert!(shared.poll(GRAPH, 0, 256, &tracks).is_none());
        check(
            &mut meter,
            MeterGraph {
                revision: 0,
                ..GRAPH
            },
            0,
            0,
        );
        check(&mut meter, MeterGraph { epoch: 1, ..GRAPH }, 0, 0);
        check(&mut meter, GRAPH, 0, MIXER_METER_MAX_TRACKS);
        check(&mut meter, GRAPH, 256, 0);
        assert!(shared.poll(GRAPH, 256, 256, &tracks).is_none());
        check(&mut meter, GRAPH, 256, MIXER_METER_MAX_TRACKS);
        let committed = shared.request.stamp.load(SeqCst);
        shared.request.stamp.store(committed + 1, SeqCst);
        check(&mut meter, GRAPH, 320, 0);
        shared.request.stamp.store(committed, SeqCst);
        assert!(shared.poll(GRAPH, 320, 256, &[]).is_none());
        check(&mut meter, GRAPH, 320, 0);
    }

    #[test]
    fn mixer_meter_concurrent_reads_keep_stereo_and_clip_with_capture_frame() {
        let shared = MixerMeterShared::default();
        let tracks = [MixerTrackId(2), MixerTrackId(9)];
        let lease = shared.request.renew(GRAPH, 0, 1_000_000, &tracks);
        let first = std::sync::Barrier::new(2);
        let check = |meter: MixerMeter| {
            assert_eq!(meter.tracks.len(), tracks.len());
            for (row, track) in meter.tracks.iter().zip(tracks) {
                assert_eq!(row.track, track);
                assert_eq!(row.peak, [meter.frames as f32, track.0 as f32]);
                assert_eq!(row.clip_frame, Some(meter.frames));
            }
        };
        std::thread::scope(|threads| {
            threads.spawn(|| {
                let mut observer = MixerMeterObserver::default();
                for block in 0..6000 {
                    let start = block * BLOCK as u64;
                    let end = start + BLOCK as u64;
                    observer.observe(&shared, GRAPH, start, |id| Some([end as f32, id.0 as f32]));
                    if block == 0 {
                        first.wait();
                        first.wait();
                    }
                }
            });
            first.wait();
            check(shared.result.read(lease, GRAPH.revision).unwrap());
            first.wait();
            for _ in 0..6000 {
                if let Some(meter) = shared.result.read(lease, GRAPH.revision) {
                    check(meter);
                }
            }
        });
    }
}
