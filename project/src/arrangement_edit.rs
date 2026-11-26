//! Explicit control-side operations. Apply to a disposable candidate, then admit/publish once.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
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
    SetAutomationPoints {
        clip: u32,
        points: Vec<AutomationPoint>,
    },
    SetAutomationClip {
        clip: u32,
        target: AutomationTarget,
        priority: u32,
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

// A flat struct streams nested ClipContent directly, regardless of kind's field order.
// An internally tagged enum would first buffer the complete operation in serde Content.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EditFields {
    kind: String,
    name: Option<String>,
    color: Option<u8>,
    track: Option<u32>,
    track_operation: Option<usize>,
    mute: Option<bool>,
    muted: Option<bool>,
    start_tick: Option<u64>,
    content: Option<ClipContent>,
    clip: Option<u32>,
    tick: Option<u64>,
    audio_first_frames: Option<u64>,
    asset: Option<u32>,
    points: Option<crate::arrangement::Points>,
    target: Option<AutomationTarget>,
    priority: Option<u32>,
    #[serde(default, deserialize_with = "read_region")]
    region: Option<Option<LoopRegion>>,
}
fn read_region<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<LoopRegion>>, D::Error> {
    Option::<LoopRegion>::deserialize(d).map(Some)
}
impl<'de> Deserialize<'de> for ArrangementEdit {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        fn need<T, E: serde::de::Error>(v: Option<T>, key: &'static str) -> Result<T, E> {
            v.ok_or_else(|| E::missing_field(key))
        }
        let f = crate::reading::named::<D, EditFields>(d)?;
        Ok(match f.kind.as_str() {
            "createTrack" => Self::CreateTrack {
                name: need(f.name, "name")?,
                color: need(f.color, "color")?,
            },
            "setTrack" => Self::SetTrack {
                track: need(f.track, "track")?,
                name: need(f.name, "name")?,
                color: need(f.color, "color")?,
                mute: need(f.mute, "mute")?,
            },
            "deleteTrack" => Self::DeleteTrack {
                track: need(f.track, "track")?,
            },
            "createClip" => Self::CreateClip {
                track: need(f.track, "track")?,
                name: need(f.name, "name")?,
                start_tick: need(f.start_tick, "startTick")?,
                muted: need(f.muted, "muted")?,
                content: need(f.content, "content")?,
            },
            "createClipOnCreatedTrack" => Self::CreateClipOnCreatedTrack {
                track_operation: need(f.track_operation, "trackOperation")?,
                name: need(f.name, "name")?,
                start_tick: need(f.start_tick, "startTick")?,
                muted: need(f.muted, "muted")?,
                content: need(f.content, "content")?,
            },
            "move" => Self::Move {
                clip: need(f.clip, "clip")?,
                track: need(f.track, "track")?,
                start_tick: need(f.start_tick, "startTick")?,
            },
            "copy" => Self::Copy {
                clip: need(f.clip, "clip")?,
                track: need(f.track, "track")?,
                start_tick: need(f.start_tick, "startTick")?,
            },
            "slice" => Self::Slice {
                clip: need(f.clip, "clip")?,
                tick: need(f.tick, "tick")?,
                audio_first_frames: f.audio_first_frames,
            },
            "resize" => Self::Resize {
                clip: need(f.clip, "clip")?,
                start_tick: need(f.start_tick, "startTick")?,
                content: need(f.content, "content")?,
            },
            "setClip" => Self::SetClip {
                clip: need(f.clip, "clip")?,
                name: need(f.name, "name")?,
                muted: need(f.muted, "muted")?,
            },
            "setAutomationPoints" => Self::SetAutomationPoints {
                clip: need(f.clip, "clip")?,
                points: need(f.points, "points")?.0,
            },
            "setAutomationClip" => Self::SetAutomationClip {
                clip: need(f.clip, "clip")?,
                target: need(f.target, "target")?,
                priority: need(f.priority, "priority")?,
            },
            "deleteClip" => Self::DeleteClip {
                clip: need(f.clip, "clip")?,
            },
            "makeUnique" => Self::MakeUnique {
                clip: need(f.clip, "clip")?,
            },
            "deleteAudioAsset" => Self::DeleteAudioAsset {
                asset: need(f.asset, "asset")?,
            },
            "createPattern" => Self::CreatePattern {
                name: need(f.name, "name")?,
            },
            "setLoop" => Self::SetLoop {
                region: need(f.region, "region")?,
            },
            _ => return Err(serde::de::Error::custom("Unknown arrangement operation")),
        })
    }
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
    let mut curve_origins = std::collections::HashMap::<usize, CurvePoints>::new();
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
            ArrangementEdit::SetAutomationPoints { clip: c, points } => {
                let i = clip(p, c)?;
                let ClipContent::Automation {
                    offset_tick,
                    length_ticks,
                    points: source,
                    ..
                } = &p.arrangement.clips[i].content
                else {
                    return Err(format!("Clip {c} is not an automation clip"));
                };
                let start = u64::from(*offset_tick);
                let end = start
                    .checked_add(*length_ticks)
                    .ok_or("Automation source range overflow")?;
                if points
                    .iter()
                    .any(|point| !(start..end).contains(&point.tick))
                {
                    return Err(format!(
                        "Clip {c}: point is outside the automation source range"
                    ));
                }
                if points.windows(2).any(|pair| pair[0].tick >= pair[1].tick) {
                    return Err(format!(
                        "Clip {c}: automation ticks must be strictly increasing"
                    ));
                }
                let first = source.partition_point(|point| point.tick < start);
                let last = first + source[first..].partition_point(|point| point.tick < end);
                if points.as_slice() == &source[first..last] {
                    continue;
                }
                // A slice or trim can leave points outside this clip's visible window.
                let mut merged = Vec::with_capacity(source.len() - (last - first) + points.len());
                merged.extend_from_slice(&source[..first]);
                merged.extend(points);
                merged.extend_from_slice(&source[last..]);
                // Copy and Slice share this allocation. Replace it everywhere on the candidate
                // so history retains the old curve and independently created curves stay separate.
                let source = source.clone();
                let origin = curve_origins
                    .remove(&source.allocation().0)
                    .unwrap_or_else(|| source.clone());
                // An edit followed by its inverse leaves no point delta. Keep the original
                // allocation so a Copy in the same batch still links after undo/redo.
                let replacement = if merged.as_slice() == &*origin {
                    origin
                } else {
                    let replacement = CurvePoints::from(merged);
                    curve_origins.insert(replacement.allocation().0, origin);
                    replacement
                };
                for sibling in &mut p.arrangement.clips {
                    if let ClipContent::Automation { points, .. } = &mut sibling.content
                        && points.shares_allocation(&source)
                    {
                        *points = replacement.clone();
                    }
                }
            }
            ArrangementEdit::SetAutomationClip {
                clip: c,
                target,
                priority,
            } => {
                let i = clip(p, c)?;
                let ClipContent::Automation {
                    target: current_target,
                    points: source,
                    ..
                } = &p.arrangement.clips[i].content
                else {
                    return Err(format!("Clip {c} is not an automation clip"));
                };
                if target != *current_target {
                    for sibling in &p.arrangement.clips {
                        if sibling.id != c
                            && let ClipContent::Automation {
                                target: sibling_target,
                                points,
                                ..
                            } = &sibling.content
                            && *sibling_target != target
                            && points.shares_allocation(source)
                        {
                            return Err(format!(
                                "Clip {c} shares its automation source with clip {} ({}) on a different target; make it unique first.",
                                sibling.id, sibling.name
                            ));
                        }
                    }
                }
                if priority > p.next_id {
                    return Err(format!(
                        "Clip {c}: automation priority {priority} exceeds the next available id {}",
                        p.next_id
                    ));
                }
                // Priorities occupy the same id space as entities and must survive Open verbatim.
                let priority = if priority == p.next_id {
                    id(p, &mut issued)?
                } else {
                    issued.insert(priority);
                    priority
                };
                let ClipContent::Automation {
                    target: clip_target,
                    priority: clip_priority,
                    ..
                } = &mut p.arrangement.clips[i].content
                else {
                    unreachable!();
                };
                *clip_target = target;
                *clip_priority = priority;
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
    SamplerTrim { channel: u32 },
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
