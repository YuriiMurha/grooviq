//! Backend batch commands. Apply them to a disposable project before publishing anything.

use crate::Project;
use crate::mixer::{
    Assignment, Insert, Level, Limits, MASTER, Mixer, Send, Sidechain, SourceKey, Tap, Track,
};
use std::collections::HashSet;

const MAX_OPERATIONS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdRef {
    Id(u32),
    /// Zero-based index of an earlier creation in this batch, of the expected entity kind.
    Created(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreatedId {
    Track(u32),
    Insert(u32),
    Send(u32),
}

#[derive(Clone, Debug)]
pub enum MixerEdit {
    CreateTrack {
        name: String,
        output: IdRef,
    },
    DeleteTrack {
        track: IdRef,
    },
    SetOutput {
        track: IdRef,
        output: IdRef,
    },
    Assign {
        source: SourceKey,
        destination: IdRef,
    },
    SetFader {
        track: IdRef,
        level: Level,
        pan: f32,
    },
    AddInsert {
        track: IdRef,
        index: usize,
        device: String,
        params: Vec<f32>,
        preset: Option<String>,
    },
    RemoveInsert {
        insert: IdRef,
    },
    MoveInsert {
        insert: IdRef,
        track: IdRef,
        index: usize,
    },
    SetInsert {
        insert: IdRef,
        params: Vec<f32>,
        preset: Option<String>,
    },
    CreateSend {
        from: IdRef,
        to: IdRef,
        tap: Tap,
        gain: f32,
    },
    SetSend {
        send: IdRef,
        from: IdRef,
        to: IdRef,
        tap: Tap,
        gain: f32,
    },
    DeleteSend {
        send: IdRef,
    },
    SetSidechain {
        previous: Option<Sidechain>,
        from: IdRef,
        to: IdRef,
        tap: Tap,
    },
    DeleteSidechain {
        sidechain: Sidechain,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixerView {
    pub generation: u64,
    pub revision: u64,
    pub mixer: Mixer,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixerAck {
    pub state: MixerView,
    pub created_ids: Vec<(usize, CreatedId)>,
    pub changed: bool,
}

fn track(m: &Mixer, id: u32) -> Result<usize, String> {
    m.tracks
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| format!("Unknown mixer track {id}"))
}

fn insert(m: &Mixer, id: u32) -> Result<(usize, usize), String> {
    m.tracks
        .iter()
        .enumerate()
        .find_map(|(t, track)| {
            track
                .inserts
                .iter()
                .position(|i| i.id == id)
                .map(|i| (t, i))
        })
        .ok_or_else(|| format!("Unknown mixer insert {id}"))
}

fn send(m: &Mixer, id: u32) -> Result<usize, String> {
    m.sends
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| format!("Unknown mixer send {id}"))
}

fn sidechain(m: &Mixer, value: &Sidechain) -> Result<usize, String> {
    m.sidechains
        .iter()
        .position(|s| s == value)
        .ok_or_else(|| "Unknown mixer sidechain".into())
}

// SplitMix64 spreads monotonically allocated IDs across the seed bits. Algorithmic Reverb
// uses only the low 16 bits, and hydrated inserts must keep whatever seed they stored.
fn insert_seed(id: u32) -> u64 {
    let mut z = u64::from(id).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn check_label(name: &str) -> Result<(), String> {
    if name.len() > Limits::DEFAULT.name_bytes {
        Err("Mixer name or preset label is too long".into())
    } else {
        Ok(())
    }
}

fn check_params(device: &str, params: &[f32], preset: Option<&str>) -> Result<(), String> {
    let spec = grooviq_dsp::param_spec(device)
        .filter(|_| crate::mixer::EFFECTS.contains(&device))
        .ok_or("Unknown mixer effect")?;
    check_label(preset.unwrap_or_default())?;
    if params.len() != spec.len()
        || !params
            .iter()
            .zip(spec)
            .all(|(value, &(min, max, _))| (min..=max).contains(value))
    {
        return Err("Invalid mixer effect parameters".into());
    }
    Ok(())
}

fn check_gain(gain: f32) -> Result<(), String> {
    if (0.0..=crate::mixer::MAX_SEND_GAIN).contains(&gain) {
        Ok(())
    } else {
        Err("Invalid mixer send gain".into())
    }
}

/// Applies a bounded batch to a disposable candidate. A caller discards the candidate on error.
pub fn apply(
    candidate: &mut Project,
    ops: Vec<MixerEdit>,
) -> Result<Vec<(usize, CreatedId)>, String> {
    if ops.len() > MAX_OPERATIONS {
        return Err("Mixer operation limit exceeded".into());
    }
    let sources: HashSet<_> = candidate
        .channels
        .iter()
        .map(|c| SourceKey::Channel(c.id))
        .chain(
            candidate
                .arrangement
                .tracks
                .iter()
                .map(|t| SourceKey::AudioTrack(t.id)),
        )
        .collect();
    let m = &mut candidate.mixer;
    let mut created = vec![None; ops.len()];
    for (operation, op) in ops.into_iter().enumerate() {
        let resolve = |reference, kind: fn(CreatedId) -> Option<u32>| match reference {
            IdRef::Id(id) => Ok(id),
            IdRef::Created(at) => created
                .get(at)
                .copied()
                .flatten()
                .and_then(kind)
                .ok_or_else(|| format!("Invalid mixer creation reference {at}")),
        };
        let track_id = |reference| {
            let id = resolve(reference, |id| match id {
                CreatedId::Track(id) => Some(id),
                _ => None,
            })?;
            track(m, id).map(|_| id)
        };
        let insert_id = |reference| {
            resolve(reference, |id| match id {
                CreatedId::Insert(id) => Some(id),
                _ => None,
            })
        };
        let send_id = |reference| {
            resolve(reference, |id| match id {
                CreatedId::Send(id) => Some(id),
                _ => None,
            })
        };
        created[operation] = match op {
            MixerEdit::CreateTrack { name, output } => {
                check_label(&name)?;
                let output = track_id(output)?;
                let id = m.take_track_id().map_err(|e| e.to_string())?;
                m.tracks.push(Track {
                    id,
                    name,
                    output: Some(output),
                    ..Track::master()
                });
                Some(CreatedId::Track(id))
            }
            MixerEdit::DeleteTrack { track: reference } => {
                let id = track_id(reference)?;
                let at = track(m, id)?;
                let output = m.tracks[at].output.ok_or("Cannot delete the master")?;
                let removed: HashSet<_> = m
                    .tracks
                    .remove(at)
                    .inserts
                    .into_iter()
                    .map(|i| i.id)
                    .collect();
                for t in &mut m.tracks {
                    if t.output == Some(id) {
                        t.output = Some(output);
                    }
                }
                m.sends.retain(|s| s.from != id && s.to != id);
                m.sidechains
                    .retain(|s| s.from != id && !removed.contains(&s.to));
                m.sources.retain(|s| s.destination != id);
                None
            }
            MixerEdit::SetOutput {
                track: reference,
                output,
            } => {
                let id = track_id(reference)?;
                let at = track(m, id)?;
                let output = track_id(output)?;
                if id == MASTER {
                    return Err("The master has no output route".into());
                }
                m.tracks[at].output = Some(output);
                None
            }
            MixerEdit::Assign {
                source,
                destination,
            } => {
                if !sources.contains(&source) {
                    return Err(format!("Unknown mixer source {source:?}"));
                }
                let destination = track_id(destination)?;
                let at = m.sources.iter().position(|a| a.source == source);
                match (at, destination) {
                    (Some(at), MASTER) => {
                        m.sources.remove(at);
                    }
                    (Some(at), _) => m.sources[at].destination = destination,
                    (None, MASTER) => {}
                    (None, _) => m.sources.push(Assignment {
                        source,
                        destination,
                    }),
                }
                None
            }
            MixerEdit::SetFader {
                track: reference,
                level,
                pan,
            } => {
                let at = track(m, track_id(reference)?)?;
                let (min, max) = crate::mixer::LEVEL_DB;
                if matches!(level, Level::Db(db) if !(db > min && db <= max))
                    || !(-1.0..=1.0).contains(&pan)
                {
                    return Err("Invalid mixer level or pan".into());
                }
                m.tracks[at].level = level;
                m.tracks[at].pan = pan;
                None
            }
            MixerEdit::AddInsert {
                track: reference,
                index,
                device,
                params,
                preset,
            } => {
                let at = track(m, track_id(reference)?)?;
                check_params(&device, &params, preset.as_deref())?;
                if index > m.tracks[at].inserts.len() {
                    return Err("Mixer insert position is out of range".into());
                }
                let id = m.take_insert_id().map_err(|e| e.to_string())?;
                m.tracks[at].inserts.insert(
                    index,
                    Insert {
                        id,
                        device,
                        params,
                        preset,
                        seed: insert_seed(id),
                        bypass: false,
                    },
                );
                Some(CreatedId::Insert(id))
            }
            MixerEdit::RemoveInsert { insert: reference } => {
                let id = insert_id(reference)?;
                let (t, i) = insert(m, id)?;
                m.tracks[t].inserts.remove(i);
                m.sidechains.retain(|s| s.to != id);
                None
            }
            MixerEdit::MoveInsert {
                insert: reference,
                track: dest,
                index,
            } => {
                let (t, i) = insert(m, insert_id(reference)?)?;
                let dest = track(m, track_id(dest)?)?;
                if index > m.tracks[dest].inserts.len() - usize::from(dest == t) {
                    return Err("Mixer insert position is out of range".into());
                }
                let moved = m.tracks[t].inserts.remove(i);
                m.tracks[dest].inserts.insert(index, moved);
                None
            }
            MixerEdit::SetInsert {
                insert: reference,
                params,
                preset,
            } => {
                let (t, i) = insert(m, insert_id(reference)?)?;
                let i = &mut m.tracks[t].inserts[i];
                check_params(&i.device, &params, preset.as_deref())?;
                i.params = params;
                i.preset = preset;
                None
            }
            MixerEdit::CreateSend {
                from,
                to,
                tap,
                gain,
            } => {
                let (from, to) = (track_id(from)?, track_id(to)?);
                check_gain(gain)?;
                let id = m.take_send_id().map_err(|e| e.to_string())?;
                m.sends.push(Send {
                    id,
                    from,
                    to,
                    tap,
                    gain,
                });
                Some(CreatedId::Send(id))
            }
            MixerEdit::SetSend {
                send: reference,
                from,
                to,
                tap,
                gain,
            } => {
                let id = send_id(reference)?;
                let at = send(m, id)?;
                let (from, to) = (track_id(from)?, track_id(to)?);
                check_gain(gain)?;
                m.sends[at] = Send {
                    id,
                    from,
                    to,
                    tap,
                    gain,
                };
                None
            }
            MixerEdit::DeleteSend { send: reference } => {
                let at = send(m, send_id(reference)?)?;
                m.sends.remove(at);
                None
            }
            MixerEdit::SetSidechain {
                previous,
                from,
                to,
                tap,
            } => {
                let (from, to) = (track_id(from)?, insert_id(to)?);
                insert(m, to)?;
                let updated = Sidechain { from, to, tap };
                if let Some(previous) = previous {
                    let at = sidechain(m, &previous)?;
                    m.sidechains[at] = updated;
                } else {
                    m.sidechains.push(updated);
                }
                None
            }
            MixerEdit::DeleteSidechain { sidechain: value } => {
                let at = sidechain(m, &value)?;
                m.sidechains.remove(at);
                None
            }
        };
    }
    m.refresh_mode();
    candidate.validate_mixer().map_err(|e| e.to_string())?;
    Ok(created
        .into_iter()
        .enumerate()
        .filter_map(|(at, id)| id.map(|id| (at, id)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_inserts_use_splitmix64_reference_seeds() {
        let mut project = Project::new();
        let ops = (0..2)
            .map(|index| MixerEdit::AddInsert {
                track: IdRef::Id(MASTER),
                index,
                device: "utility".into(),
                params: vec![0.0, 0.0, 1.0, 0.0, 0.0],
                preset: None,
            })
            .collect();
        apply(&mut project, ops).unwrap();
        let seeds = project.mixer.tracks[0]
            .inserts
            .iter()
            .map(|insert| insert.seed)
            .collect::<Vec<_>>();
        assert_eq!(seeds, [0x910a_2dec_8902_5cc1, 0x9758_35de_1c97_56ce]);
        assert_eq!(insert_seed(u32::MAX - 1), 0x0f90_d8fa_cbff_ba22);
    }
}
