//! Explicit control-side operations. Apply to a disposable candidate, then admit/publish once.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ArrangementEdit {
    CreateTrack {
        name: String,
        color: u8,
    },
    SetTrack {
        track: u32,
        name: String,
        color: u8,
        mute: bool,
    },
    DeleteTrack {
        track: u32,
    },
    CreateClip {
        track: u32,
        name: String,
        start_tick: u64,
        muted: bool,
        content: ClipContent,
    },
    CreateClipOnCreatedTrack {
        track_operation: usize,
        name: String,
        start_tick: u64,
        muted: bool,
        content: ClipContent,
    },
    Move {
        clip: u32,
        track: u32,
        start_tick: u64,
    },
    Copy {
        clip: u32,
        track: u32,
        start_tick: u64,
    },
    /// Musical cut is an absolute tick. Audio also requires an explicit count of source
    /// frames in the first chronological fragment and an explicit second onset tick.
    Slice {
        clip: u32,
        tick: u64,
        audio_first_frames: Option<u64>,
    },
    Resize {
        clip: u32,
        start_tick: u64,
        content: ClipContent,
    },
    SetClip {
        clip: u32,
        name: String,
        muted: bool,
    },
    DeleteClip {
        clip: u32,
    },
    MakeUnique {
        clip: u32,
    },
    DeleteAudioAsset {
        asset: u32,
    },
    CreatePattern {
        name: String,
    },
    SetLoop {
        region: Option<LoopRegion>,
    },
}
fn id(p: &mut Project, issued: &mut std::collections::HashSet<u32>) -> Result<u32, String> {
    while p.next_id < u32::MAX {
        let n = p.next_id;
        p.next_id += 1;
        if issued.insert(n) {
            return Ok(n);
        }
    }
    Err("Project ids exhausted".into())
}
fn clip(p: &Project, id: u32) -> Result<usize, String> {
    p.arrangement
        .clips
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| format!("Unknown clip {id}"))
}
fn track(p: &Project, id: u32) -> Result<usize, String> {
    p.arrangement
        .tracks
        .iter()
        .position(|t| t.id == id)
        .ok_or_else(|| format!("Unknown track {id}"))
}
fn fresh_priority(
    p: &mut Project,
    content: &mut ClipContent,
    issued: &mut std::collections::HashSet<u32>,
) -> Result<(), String> {
    if let ClipContent::Automation { priority, .. } = content {
        *priority = id(p, issued)?;
    }
    Ok(())
}
/// IDs returned in operation order; all allocations occur on the candidate only.
pub fn apply(p: &mut Project, ops: Vec<ArrangementEdit>) -> Result<Vec<u32>, String> {
    if ops.len() > MAX_CLIPS {
        return Err("Arrangement operation limit exceeded".into());
    }
    let mut issued: std::collections::HashSet<_> = p
        .channels
        .iter()
        .map(|c| c.id)
        .chain(p.patterns.iter().map(|p| p.id))
        .chain(p.arrangement.ids())
        .collect();
    let mut ids = Vec::new();
    let mut tracks_by_operation = std::collections::HashMap::new();
    for (operation, op) in ops.into_iter().enumerate() {
        match op {
            ArrangementEdit::CreateTrack { name, color } => {
                let n = id(p, &mut issued)?;
                p.arrangement.tracks.push(PlaylistTrack {
                    id: n,
                    name,
                    color,
                    mute: false,
                });
                ids.push(n);
                tracks_by_operation.insert(operation, n);
            }
            ArrangementEdit::SetTrack {
                track: t,
                name,
                color,
                mute,
            } => {
                let i = track(p, t)?;
                p.arrangement.tracks[i] = PlaylistTrack {
                    id: t,
                    name,
                    color,
                    mute,
                };
            }
            ArrangementEdit::DeleteTrack { track: t } => {
                let i = track(p, t)?;
                p.arrangement.tracks.remove(i);
            }
            ArrangementEdit::CreateClip {
                track: t,
                name,
                start_tick,
                muted,
                mut content,
            } => {
                track(p, t)?;
                let n = id(p, &mut issued)?;
                fresh_priority(p, &mut content, &mut issued)?;
                p.arrangement.clips.push(Clip {
                    id: n,
                    track: t,
                    name,
                    start_tick,
                    muted,
                    content,
                });
                ids.push(n);
            }
            ArrangementEdit::CreateClipOnCreatedTrack {
                track_operation,
                name,
                start_tick,
                muted,
                mut content,
            } => {
                let t = *tracks_by_operation
                    .get(&track_operation)
                    .ok_or("Track operation must refer to an earlier CreateTrack")?;
                let n = id(p, &mut issued)?;
                fresh_priority(p, &mut content, &mut issued)?;
                p.arrangement.clips.push(Clip {
                    id: n,
                    track: t,
                    name,
                    start_tick,
                    muted,
                    content,
                });
                ids.push(n);
            }
            ArrangementEdit::Move {
                clip: c,
                track: t,
                start_tick,
            } => {
                track(p, t)?;
                let i = clip(p, c)?;
                p.arrangement.clips[i].track = t;
                p.arrangement.clips[i].start_tick = start_tick;
            }
            ArrangementEdit::Copy {
                clip: c,
                track: t,
                start_tick,
            } => {
                track(p, t)?;
                let mut c = p.arrangement.clips[clip(p, c)?].clone();
                c.id = id(p, &mut issued)?;
                fresh_priority(p, &mut c.content, &mut issued)?;
                c.track = t;
                c.start_tick = start_tick;
                ids.push(c.id);
                p.arrangement.clips.push(c);
            }
            ArrangementEdit::Slice {
                clip: c,
                tick,
                audio_first_frames,
            } => {
                let i = clip(p, c)?;
                let mut right = p.arrangement.clips[i].clone();
                let cut = tick
                    .checked_sub(right.start_tick)
                    .filter(|c| *c > 0)
                    .ok_or("Slice must be inside clip")?;
                let mut left = right.clone();
                match (&mut left.content, &mut right.content) {
                    (
                        ClipContent::Pattern {
                            pattern,
                            offset_tick,
                            length_ticks,
                        },
                        ClipContent::Pattern {
                            offset_tick: r,
                            length_ticks: len,
                            ..
                        },
                    ) => {
                        if audio_first_frames.is_some() || cut >= *length_ticks {
                            return Err("Invalid musical slice".into());
                        }
                        let source = p
                            .patterns
                            .iter()
                            .find(|p| p.id == *pattern)
                            .ok_or("Unknown pattern")?;
                        *r = ((u64::from(*offset_tick) + cut)
                            % u64::from(source.steps * grooviq_engine::STEP_TICKS))
                            as u32;
                        *len -= cut;
                        *length_ticks = cut;
                    }
                    (
                        ClipContent::Automation {
                            offset_tick,
                            length_ticks,
                            ..
                        },
                        ClipContent::Automation {
                            offset_tick: r,
                            length_ticks: len,
                            ..
                        },
                    ) => {
                        if audio_first_frames.is_some() || cut >= *length_ticks {
                            return Err("Invalid automation slice".into());
                        }
                        *r = u32::try_from(u64::from(*offset_tick) + cut)
                            .map_err(|_| "Source offset overflow")?;
                        *len -= cut;
                        *length_ticks = cut;
                    }
                    (
                        ClipContent::Audio {
                            asset,
                            source_start,
                            source_end,
                            reverse,
                            fade_in,
                            fade_out,
                            ..
                        },
                        ClipContent::Audio {
                            source_start: rs,
                            source_end: re,
                            fade_in: fi,
                            fade_out: fo,
                            ..
                        },
                    ) => {
                        let count = audio_first_frames
                            .ok_or("Audio slice requires explicit source-frame count")?;
                        let length = source_end
                            .checked_sub(*source_start)
                            .ok_or("Invalid source bounds")?;
                        if count == 0 || count >= length {
                            return Err("Audio cut must partition source frames".into());
                        }
                        let source = p
                            .arrangement
                            .audio_assets
                            .iter()
                            .find(|a| a.id == *asset)
                            .ok_or("Unknown audio asset")?;
                        let cut_tick = (count as f64 / f64::from(source.source_rate)
                            * f64::from(p.bpm)
                            * 960.0
                            / 60.0)
                            .ceil() as u64;
                        if cut != cut_tick {
                            return Err("Audio cut tick must equal start plus ceil(source frames × BPM × 960 / (source rate × 60))".into());
                        }
                        if *reverse {
                            let edge = *source_end - count;
                            *source_start = edge;
                            *re = edge;
                        } else {
                            let edge = *source_start + count;
                            *source_end = edge;
                            *rs = edge;
                        }
                        // No fractional fade fields in v3: clamp only the outer fades; the new
                        // inner boundary has no fade. Undo restores the original exactly.
                        *fade_in = (*fade_in).min(count);
                        *fade_out = 0;
                        *fi = 0;
                        *fo = (*fo).min(length - count);
                    }
                    _ => unreachable!(),
                }
                right.id = id(p, &mut issued)?;
                right.start_tick = tick;
                ids.push(right.id);
                p.arrangement.clips[i] = left;
                p.arrangement.clips.push(right);
            }
            ArrangementEdit::Resize {
                clip: c,
                start_tick,
                mut content,
            } => {
                let i = clip(p, c)?;
                let old = &p.arrangement.clips[i].content;
                // Explicit source offsets/bounds; resize cannot retarget, change source or priority.
                let compatible = match (old, &content) {
                    (
                        ClipContent::Pattern { pattern: a, .. },
                        ClipContent::Pattern { pattern: b, .. },
                    ) => a == b,
                    (ClipContent::Audio { asset: a, .. }, ClipContent::Audio { asset: b, .. }) => {
                        a == b
                    }
                    (
                        ClipContent::Automation {
                            target: a,
                            priority: x,
                            points: p,
                            ..
                        },
                        ClipContent::Automation {
                            target: b,
                            priority: y,
                            points: q,
                            ..
                        },
                    ) => a == b && x == y && p == q,
                    _ => false,
                };
                if !compatible {
                    return Err("Resize must preserve source, target and priority".into());
                }
                let shift = i128::from(start_tick) - i128::from(p.arrangement.clips[i].start_tick);
                match (old, &mut content) {
                    (
                        ClipContent::Pattern {
                            pattern,
                            offset_tick,
                            ..
                        },
                        ClipContent::Pattern {
                            offset_tick: new, ..
                        },
                    ) => {
                        let source = p
                            .patterns
                            .iter()
                            .find(|p| p.id == *pattern)
                            .ok_or("Unknown pattern")?;
                        let length = i128::from(source.steps * grooviq_engine::STEP_TICKS);
                        if i128::from(*new) != (i128::from(*offset_tick) + shift).rem_euclid(length)
                        {
                            return Err("Resize must preserve pattern source phase".into());
                        }
                    }
                    (
                        ClipContent::Automation {
                            offset_tick,
                            points,
                            ..
                        },
                        ClipContent::Automation {
                            offset_tick: new,
                            points: shared,
                            ..
                        },
                    ) => {
                        if i128::from(*new) != i128::from(*offset_tick) + shift {
                            return Err("Resize must preserve automation source phase".into());
                        }
                        *shared = points.clone();
                    }
                    _ => {}
                }
                p.arrangement.clips[i].start_tick = start_tick;
                p.arrangement.clips[i].content = content;
            }
            ArrangementEdit::SetClip {
                clip: c,
                name,
                muted,
            } => {
                let i = clip(p, c)?;
                p.arrangement.clips[i].name = name;
                p.arrangement.clips[i].muted = muted;
            }
            ArrangementEdit::DeleteClip { clip: c } => {
                let i = clip(p, c)?;
                p.arrangement.clips.remove(i);
            }
            ArrangementEdit::MakeUnique { clip: c } => {
                ids.push(
                    crate::pattern_edit::apply(
                        p,
                        crate::pattern_edit::PatternEdit::MakeUnique { clip: c },
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
            ArrangementEdit::CreatePattern { name } => {
                ids.push(
                    crate::pattern_edit::apply(
                        p,
                        crate::pattern_edit::PatternEdit::Create { name },
                    )
                    .map_err(|e| e.to_string())?,
                );
            }
            ArrangementEdit::DeleteAudioAsset { asset } => {
                let i = p
                    .arrangement
                    .audio_assets
                    .iter()
                    .position(|a| a.id == asset)
                    .ok_or("Unknown audio asset")?;
                p.arrangement.audio_assets.remove(i);
            }
            ArrangementEdit::SetLoop { region } => p.arrangement.loop_region = region,
        }
    }
    p.validate_arrangement()?;
    Ok(ids)
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrangementView {
    pub generation: u64,
    pub revision: u64,
    pub arrangement: Arrangement,
    pub next_id: u32,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrangementAck {
    pub state: ArrangementView,
    pub created_ids: Vec<u32>,
    pub changed: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HistoryEffect {
    Arrangement,
    Notes { pattern: u32 },
    Empty,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAck {
    pub effect: HistoryEffect,
    pub state: ArrangementView,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrangementRefusal {
    pub message: String,
    pub reload: bool,
    pub state: Box<ArrangementView>,
}

/// IPC passes a bounded JSON string so the point/count visitors run before materializing
/// the request. Tauri must not deserialize a serde_json::Value tree first.
pub fn parse_batch(json: &str) -> Result<Vec<ArrangementEdit>, String> {
    if json.len() > 64 * 1024 * 1024 {
        return Err("Arrangement request exceeds 64 MiB".into());
    }
    struct Batch;
    impl<'de> serde::de::Visitor<'de> for Batch {
        type Value = Vec<ArrangementEdit>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(f, "bounded arrangement batch")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut a: A,
        ) -> Result<Self::Value, A::Error> {
            let mut ops = Vec::new();
            loop {
                if ops.len() == crate::MAX_CLIPS {
                    if a.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(serde::de::Error::custom("Arrangement operation limit"));
                    }
                    break;
                }
                let Some(op) = a.next_element()? else {
                    break;
                };
                ops.push(op);
            }
            Ok(ops)
        }
    }
    use serde::de::Deserializer;
    crate::reading::with_context(crate::VERSION, Default::default(), || {
        let mut d = serde_json::Deserializer::from_str(json);
        let ops = (&mut d).deserialize_seq(Batch).map_err(|e| e.to_string())?;
        d.end().map_err(|e| e.to_string())?;
        Ok(ops)
    })
    .0
}
