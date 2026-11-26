//! Control-side translation of a supported project mixer into the live engine's setup.

use crate::Project;
use crate::mixer::{Level, MixerError, Mode, SourceKey, Tap};
use grooviq_engine::mixer as engine;
use std::collections::HashMap;

fn tap(tap: Tap) -> engine::Tap {
    match tap {
        Tap::Pre => engine::Tap::PreFader,
        Tap::Post => engine::Tap::PostFader,
    }
}

/// Validate the complete stored mixer and expand it over the project's effective sources.
pub fn compile_mix(project: &Project) -> Result<engine::MixSetup, MixerError> {
    project.validate_mixer()?;
    let mixer = &project.mixer;
    if mixer.mode == Mode::LegacyDirect {
        return Ok(engine::MixSetup::LegacyDirect);
    }
    for track in &mixer.tracks {
        if track.mute || track.solo {
            return Err(MixerError::UnsupportedTrackControls(track.id));
        }
        if let Some(insert) = track.inserts.iter().find(|i| i.bypass) {
            return Err(MixerError::UnsupportedBypass(insert.id));
        }
    }
    let mut tracks = Vec::with_capacity(mixer.tracks.len());
    let mut gains = Vec::with_capacity(mixer.tracks.len());
    let mut devices = Vec::new();
    for track in &mixer.tracks {
        let mut inserts = Vec::with_capacity(track.inserts.len());
        for insert in &track.inserts {
            let id = engine::InsertId(insert.id.into());
            inserts.push(
                engine::describe_builtin_insert(id, &insert.device)
                    .ok_or(MixerError::UnsupportedInsert(insert.id))?,
            );
            devices.push(engine::InsertDevice {
                insert: id,
                kind: insert.device.clone(),
                seed: insert.seed,
                params: insert.params.clone(),
            });
        }
        let id = engine::MixerTrackId(track.id.into());
        tracks.push(engine::Track {
            id,
            output: track.output.map(|id| engine::MixerTrackId(id.into())),
            inserts,
        });
        let volume = match track.level {
            Level::Db(db) => grooviq_dsp::db_to_gain(db),
            Level::Silence => 0.0,
        };
        let [left, right] = grooviq_engine::ChannelMix {
            volume,
            pan: track.pan,
            ..Default::default()
        }
        .gains(true);
        gains.push(engine::TrackGain {
            track: id,
            left,
            right,
        });
    }
    let sends = mixer
        .sends
        .iter()
        .map(|s| engine::Send {
            id: engine::SendId(s.id.into()),
            from: engine::MixerTrackId(s.from.into()),
            to: engine::MixerTrackId(s.to.into()),
            tap: tap(s.tap),
            gain: s.gain,
        })
        .collect();
    let sidechains = mixer
        .sidechains
        .iter()
        .map(|s| engine::Sidechain {
            from: engine::MixerTrackId(s.from.into()),
            to: engine::InsertId(s.to.into()),
            tap: tap(s.tap),
        })
        .collect();

    let assigned: HashMap<_, _> = mixer
        .sources
        .iter()
        .map(|a| (a.source, engine::MixerTrackId(a.destination.into())))
        .collect();
    let destination = |source| assigned.get(&source).copied().unwrap_or(engine::MASTER);
    let mut sources =
        Vec::with_capacity(project.channels.len() + project.arrangement.tracks.len() + 2);
    sources.extend(project.channels.iter().map(|c| engine::Source {
        id: engine::SourceId::Channel(c.id.into()),
        destination: destination(SourceKey::Channel(c.id)),
    }));
    sources.extend(project.arrangement.tracks.iter().map(|t| engine::Source {
        id: engine::SourceId::AudioTrack(t.id.into()),
        destination: destination(SourceKey::AudioTrack(t.id)),
    }));
    sources.extend(
        [engine::SystemSource::Tone, engine::SystemSource::Synth].map(|source| engine::Source {
            id: engine::SourceId::System(source),
            destination: engine::MASTER,
        }),
    );
    let mix = engine::MixSetup::Routed(engine::RoutedMix {
        mixer: engine::Mixer {
            tracks,
            sends,
            sidechains,
            sources,
        },
        gains,
        devices,
    });
    mix.validate_for_channels(
        project
            .channels
            .iter()
            .map(|c| grooviq_engine::ChannelId(c.id)),
    )
    .map_err(|err| MixerError::Graph(format!("live mixer admission: {err}")))?;
    Ok(mix)
}
