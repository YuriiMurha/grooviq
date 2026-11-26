//! Control-side adapter from persisted arrangement identities to immutable engine schedules.
use crate::{AutomationTarget, ClipContent, Project, SegmentMode, compile_pattern};
use grooviq_dsp::{sampler, sampler::SampleBuf, synth};
use grooviq_engine::{self as engine, ChannelId, InstrumentKind, SongData};
use std::{collections::HashMap, sync::Arc};

/// Reuse the previous audio bank only when the caller has proved that audio geometry,
/// track audibility and decoded allocation identities are unchanged.
pub fn compile_song(
    project: &Project,
    decoded: &HashMap<u32, Arc<SampleBuf>>,
    unchanged_audio: Option<&engine::AudioSchedule>,
    max_notes: usize,
) -> Result<SongData, String> {
    crate::mixer::require_runtime_support(project).map_err(|e| e.to_string())?;
    project.validate_arrangement()?;
    if !project.bpm.is_finite() || !(20.0..=400.0).contains(&project.bpm) {
        return Err("Song tempo must be between 20 and 400 BPM".into());
    }
    let tracks: HashMap<_, _> = project
        .arrangement
        .tracks
        .iter()
        .map(|t| (t.id, t.mute))
        .collect();
    let assets: HashMap<_, _> = project
        .arrangement
        .audio_assets
        .iter()
        .map(|a| (a.id, a))
        .collect();
    let audible = |clip: &crate::Clip| !clip.muted && tracks.get(&clip.track) == Some(&false);
    let patterns: Vec<_> = project
        .patterns
        .iter()
        .map(|p| (p.id, compile_pattern(project, p)))
        .collect();
    let clips: Vec<_> = project
        .arrangement
        .clips
        .iter()
        .filter_map(|c| {
            if let ClipContent::Pattern {
                pattern,
                offset_tick,
                length_ticks,
            } = c.content
            {
                Some(engine::PatternClip {
                    id: c.id,
                    pattern,
                    start: c.start_tick,
                    length: length_ticks,
                    offset: offset_tick,
                    muted: !audible(c),
                })
            } else {
                None
            }
        })
        .collect();
    let arrangement = engine::compile_pattern_clips(&patterns, &clips, max_notes).map_err(|e| {
        format!("Song expansion refused; shorten clips or remove repetitions: {e:?}")
    })?;
    let audio = if let Some(bank) = unchanged_audio {
        bank.clone()
    } else {
        let assets = project
            .arrangement
            .audio_assets
            .iter()
            .map(|a| engine::AudioAsset {
                id: a.id,
                source_rate: a.source_rate,
                source_frames: a.source_frames,
                decoded: decoded.get(&a.id).cloned(),
            })
            .collect();
        let clips = project
            .arrangement
            .clips
            .iter()
            .filter(|c| audible(c))
            .filter_map(|c| {
                if let ClipContent::Audio {
                    asset,
                    source_start,
                    source_end,
                    gain,
                    reverse,
                    fade_in,
                    fade_out,
                } = c.content
                {
                    Some(engine::AudioClip {
                        id: c.id,
                        asset,
                        start_tick: c.start_tick,
                        source_start,
                        source_end,
                        gain,
                        reverse,
                        fade_in,
                        fade_out,
                    })
                } else {
                    None
                }
            })
            .collect();
        engine::AudioSchedule::build(assets, clips)
            .map_err(|e| e.to_string())?
            .0
    };
    // Routes live outside the (possibly reused) bank, so a track-only move refreshes them
    // while every sounding clip keeps its place.
    let routes: Vec<_> = project
        .arrangement
        .clips
        .iter()
        .filter(|c| matches!(c.content, ClipContent::Audio { .. }))
        .map(|c| (c.id, u64::from(c.track)))
        .collect();
    let audio = audio.with_tracks(&routes).map_err(|e| e.to_string())?;
    let mut curves = Vec::new();
    for c in project.arrangement.clips.iter().filter(|c| audible(c)) {
        if let ClipContent::Automation {
            target,
            priority,
            offset_tick,
            length_ticks,
            points,
        } = &c.content
        {
            let target = match target {
                AutomationTarget::ChannelVolume { channel } => {
                    engine::AutomationTarget::ChannelVolume(ChannelId(*channel))
                }
                AutomationTarget::ChannelPan { channel } => {
                    engine::AutomationTarget::ChannelPan(ChannelId(*channel))
                }
                AutomationTarget::InstrumentParam {
                    channel,
                    instrument,
                    param,
                } => {
                    let (kind, ids): (_, &[&str]) = match instrument.as_str() {
                        "sampler" => (InstrumentKind::Sampler, &sampler::PARAM_IDS),
                        "synth" => (InstrumentKind::Synth, &synth::PARAM_IDS),
                        _ => return Err("Unknown automation instrument".into()),
                    };
                    let index = ids
                        .iter()
                        .position(|id| *id == param)
                        .ok_or("Unknown automation parameter")?;
                    engine::AutomationTarget::InstrumentParam {
                        channel: ChannelId(*channel),
                        instrument: kind,
                        param: index as u32,
                    }
                }
            };
            curves.push(engine::AutomationClip {
                target,
                priority: *priority,
                start_tick: c.start_tick,
                offset_tick: *offset_tick,
                length_ticks: *length_ticks,
                points: points
                    .iter()
                    .map(|p| engine::AutomationPoint {
                        tick: p.tick as u32,
                        value: p.value,
                        segment: match p.mode {
                            SegmentMode::Linear => engine::AutomationSegment::Linear,
                            SegmentMode::Hold => engine::AutomationSegment::Hold,
                        },
                    })
                    .collect(),
            });
        }
    }
    let channels = crate::compile::channel_setups(project, decoded);
    let automation =
        engine::AutomationSchedule::build(curves, &channels).map_err(|e| e.to_string())?;
    let mut musical_end = arrangement.length;
    let mut end_tick = musical_end;
    let frame_rate = 48_000.0;
    let mut audio_frame_end = 0;
    let mut audio_extents = Vec::new();
    for c in &project.arrangement.clips {
        match c.content {
            ClipContent::Pattern { length_ticks, .. }
            | ClipContent::Automation { length_ticks, .. } => {
                musical_end = musical_end.max(
                    c.start_tick
                        .checked_add(length_ticks)
                        .ok_or("Song horizon overflow")?,
                );
            }
            ClipContent::Audio {
                asset,
                source_start,
                source_end,
                ..
            } => {
                let source = assets.get(&asset).ok_or("Missing audio asset")?;
                audio_extents.push((c.start_tick, source_end - source_start, source.source_rate));
                let seconds = (source_end - source_start) as f64 / f64::from(source.source_rate);
                let ticks = (seconds * f64::from(project.bpm) * 960.0 / 60.0).ceil() as u64;
                end_tick = end_tick.max(
                    c.start_tick
                        .checked_add(ticks)
                        .ok_or("Song horizon overflow")?,
                );
                audio_frame_end = audio_frame_end.max(
                    engine::ticks_to_frames(c.start_tick as f64, project.bpm, frame_rate)
                        + ((source_end - source_start) as f64 * f64::from(frame_rate)
                            / f64::from(source.source_rate))
                        .ceil() as u64,
                );
            }
        }
    }
    end_tick = end_tick.max(musical_end);
    if end_tick > engine::MAX_SONG_TICKS {
        return Err("Song exceeds its current-tempo horizon; shorten or move clips".into());
    }
    let end_frame =
        engine::ticks_to_frames(musical_end as f64, project.bpm, frame_rate).max(audio_frame_end);
    let region = project
        .arrangement
        .loop_region
        .as_ref()
        .filter(|r| r.enabled)
        .map(|r| engine::SongLoop {
            start_tick: r.start_tick,
            end_tick: r.end_tick,
        });
    if region.is_some_and(|r| r.end_tick > end_tick) {
        return Err("Song loop extends beyond the song; shorten or disable the loop".into());
    }
    audio.validate_loop(region).map_err(|e| e.to_string())?;
    Ok(SongData {
        arrangement,
        audio,
        automation,
        end_tick,
        end_frame,
        frame_rate,
        frame_extent: Some(Arc::new(engine::SongFrameExtent {
            musical_end_tick: musical_end,
            audio: audio_extents,
        })),
    })
}
