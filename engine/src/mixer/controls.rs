use super::runtime::{FaderGain, reserved};
use super::{
    CompiledMixer, Input, InputId, InsertBypass, InsertId, MixerTrackId, PrepareError, Tap,
};
use std::collections::BTreeMap;
use std::mem::size_of;

/// One assigned detector feed, independent of its source's audible outputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DetectorFeed {
    pub from: MixerTrackId,
    pub insert: InsertId,
    pub tap: Tap,
}

/// A complete set of mixer control targets. Each list is strictly ordered by its identity.
/// Keep a controlled insert in `bypass` even when wet to warm its dry history.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MixerControls {
    pub muted_tracks: Vec<MixerTrackId>,
    pub soloed_tracks: Vec<MixerTrackId>,
    pub muted_detectors: Vec<DetectorFeed>,
    pub bypass: Vec<InsertBypass>,
}

#[derive(Clone, Copy, Default)]
struct Roles {
    into_solo: bool,
    from_solo: bool,
}

#[derive(Clone, Copy)]
pub(super) struct FeedGate {
    pub(super) id: InputId,
    pub(super) gain: FaderGain,
}

impl FeedGate {
    pub(super) fn at(id: InputId, open: bool) -> Self {
        Self {
            id,
            gain: FaderGain::at(f32::from(open)),
        }
    }
}

pub(super) const GATE_TABLE_BYTES: u64 = size_of::<FeedGate>() as u64;
pub(super) const ROLE_BYTES: u64 = size_of::<Roles>() as u64;

pub(super) struct GatePlan<'a> {
    controls: &'a MixerControls,
    roles: Vec<Roles>,
}

impl<'a> GatePlan<'a> {
    pub(super) fn new(
        controls: &'a MixerControls,
        compiled: &CompiledMixer,
        order: &[usize],
        node_of: &BTreeMap<MixerTrackId, usize>,
    ) -> Result<Self, PrepareError> {
        Self::validate(controls, compiled, node_of)?;

        let mut roles = Vec::new();
        if !controls.soloed_tracks.is_empty() {
            roles = reserved(order.len())?;
            roles.extend(order.iter().map(|&i| {
                let solo = controls
                    .soloed_tracks
                    .binary_search(&compiled.tracks[i].track.id)
                    .is_ok();
                Roles {
                    into_solo: solo,
                    from_solo: solo,
                }
            }));
            let upstream = |i: usize| {
                compiled.tracks[order[i]]
                    .inputs
                    .iter()
                    .filter_map(|input| match input.id {
                        InputId::Main(_) | InputId::Send(_) => {
                            input.from.and_then(|from| node_of.get(&from).copied())
                        }
                        _ => None,
                    })
            };
            for i in (0..order.len()).rev() {
                if roles[i].into_solo {
                    for from in upstream(i) {
                        roles[from].into_solo = true;
                    }
                }
            }
            for i in 0..order.len() {
                roles[i].from_solo |= upstream(i).any(|from| roles[from].from_solo);
            }
        }
        Ok(Self { controls, roles })
    }

    pub(super) fn validate(
        controls: &MixerControls,
        compiled: &CompiledMixer,
        node_of: &BTreeMap<MixerTrackId, usize>,
    ) -> Result<(), PrepareError> {
        fn ordered<T: Ord>(targets: &[T]) -> bool {
            targets.windows(2).all(|pair| pair[0] < pair[1])
        }
        let tracks =
            |ids: &[MixerTrackId]| ordered(ids) && ids.iter().all(|id| node_of.contains_key(id));
        if !tracks(&controls.muted_tracks)
            || !tracks(&controls.soloed_tracks)
            || !ordered(&controls.muted_detectors)
        {
            return Err(PrepareError::InvalidControls);
        }

        if !controls.muted_detectors.is_empty() {
            let matched = compiled
                .tracks
                .iter()
                .flat_map(|t| &t.inputs)
                .filter(|input| {
                    key(input.id)
                        .is_some_and(|key| controls.muted_detectors.binary_search(&key).is_ok())
                })
                .count();
            if matched != controls.muted_detectors.len() {
                return Err(PrepareError::InvalidControls);
            }
        }

        Ok(())
    }

    pub(super) fn track_open(&self, track: MixerTrackId) -> bool {
        self.controls.muted_tracks.binary_search(&track).is_err()
    }

    pub(super) fn audible_open(
        &self,
        input: &Input,
        to: usize,
        node_of: &BTreeMap<MixerTrackId, usize>,
    ) -> bool {
        if input.from.is_some_and(|from| !self.track_open(from)) {
            return false;
        }
        let Some(to) = self.roles.get(to) else {
            return true;
        };
        // Tracks outside the audible selection still process their own inputs, including
        // buses used only as detectors. Their routes into a selected path are gated.
        if to.into_solo || !to.from_solo {
            return true;
        }
        input
            .from
            .is_some_and(|from| node_of.get(&from).is_some_and(|&i| self.roles[i].from_solo))
    }

    pub(super) fn detector_open(&self, id: InputId) -> bool {
        self.controls
            .muted_detectors
            .binary_search(&key(id).expect("detector identity"))
            .is_err()
    }
}

fn key(id: InputId) -> Option<DetectorFeed> {
    match id {
        InputId::Sidechain { from, insert, tap } => Some(DetectorFeed { from, insert, tap }),
        _ => None,
    }
}
