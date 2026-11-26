use crate::{Audio, lock};
use grooviq_engine::{Controller, MIXER_METER_MAX_TRACKS, mixer::MixerTrackId};
use grooviq_project::session::Session;
use serde::Serialize;
use std::sync::Mutex;
use tauri::State;

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MixerMeterReply {
    generation: u64,
    revision: u64,
    mix_revision: String,
    frames: String,
    tracks: Vec<MixerMeterRow>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MixerMeterRow {
    track: u32,
    peak: [f32; 2],
    clip_frame: Option<String>,
}

#[tauri::command]
pub(crate) async fn mixer_meter(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    tracks: Vec<u32>,
) -> Result<Option<MixerMeterReply>, String> {
    poll(
        &audio.session,
        &audio.controller,
        generation,
        revision,
        &tracks,
    )
}

fn poll(
    session: &Mutex<Session>,
    controller: &Mutex<Controller>,
    generation: u64,
    revision: u64,
    tracks: &[u32],
) -> Result<Option<MixerMeterReply>, String> {
    if generation > MAX_SAFE_INTEGER || revision > MAX_SAFE_INTEGER {
        return Err("Invalid mixer meter stamp".into());
    }
    if tracks.len() > MIXER_METER_MAX_TRACKS || tracks.contains(&0) {
        return Err("Invalid mixer meter track selection".into());
    }
    let mut ids = [MixerTrackId(0); MIXER_METER_MAX_TRACKS];
    for (id, &track) in ids.iter_mut().zip(tracks) {
        *id = MixerTrackId(u64::from(track));
    }
    let meter = {
        let session = lock(session);
        if session.mixer_stamp() != (generation, revision) {
            return Ok(None);
        }
        // Keep the stamp valid through the controller lease update.
        lock(controller).mixer_meter(&ids[..tracks.len()])
    };
    meter
        .map(|meter| {
            let tracks = meter
                .tracks
                .into_iter()
                .map(|row| {
                    Ok(MixerMeterRow {
                        track: u32::try_from(row.track.0)
                            .map_err(|_| "Invalid captured track ID")?,
                        peak: row.peak,
                        clip_frame: row.clip_frame.map(|frame| frame.to_string()),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(MixerMeterReply {
                generation,
                revision,
                mix_revision: meter.mix_revision.to_string(),
                frames: meter.frames.to_string(),
                tracks,
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use grooviq_engine::{AudioProcessor, BLOCK, ChannelId, Command, EngineConfig, NoteId, engine};
    use grooviq_project::mixer::{Level, SourceKey};
    use grooviq_project::mixer_edit::{CreatedId, IdRef, MixerEdit};
    use std::sync::Arc;
    use std::time::Instant;

    fn rig() -> (Mutex<Session>, Arc<Mutex<Controller>>, AudioProcessor) {
        let (controller, processor) = engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        let controller = Arc::new(Mutex::new(controller));
        (
            Mutex::new(Session::new(controller.clone())),
            controller,
            processor,
        )
    }

    fn track(session: &Mutex<Session>) -> u32 {
        let mut session = lock(session);
        let (generation, revision) = session.mixer_stamp();
        let ack = session
            .edit_mixer(
                generation,
                revision,
                vec![MixerEdit::CreateTrack {
                    name: "Meter".into(),
                    output: IdRef::Id(0),
                }],
                Instant::now(),
            )
            .unwrap();
        let [(0, CreatedId::Track(track))] = ack.created_ids.as_slice() else {
            panic!("expected one created track");
        };
        *track
    }

    #[test]
    fn mixer_meter_bridge_only_leases_the_current_stamp_and_reports_real_captures() {
        let (session, controller, mut processor) = rig();
        let (generation, old_revision) = lock(&session).mixer_stamp();
        let track = track(&session);
        let (channel, revision) = {
            let mut session = lock(&session);
            session.add_synth_channel("Init", Instant::now()).unwrap();
            let channel = session.project().channels[0].id;
            let (generation, revision) = session.mixer_stamp();
            session
                .edit_mixer(
                    generation,
                    revision,
                    vec![
                        MixerEdit::Assign {
                            source: SourceKey::Channel(channel),
                            destination: IdRef::Id(track),
                        },
                        MixerEdit::SetFader {
                            track: IdRef::Id(track),
                            level: Level::Db(0.0),
                            pan: -0.5,
                        },
                    ],
                    Instant::now(),
                )
                .unwrap();
            (ChannelId(channel), session.mixer_stamp().1)
        };
        let current =
            |tracks: &[u32]| poll(&session, &controller, generation, revision, tracks).unwrap();
        assert!(
            poll(&session, &controller, generation, old_revision, &[track])
                .unwrap()
                .is_none()
        );
        let mut output = [0.0; BLOCK * 2];
        processor.process(&mut output, 2);
        assert!(current(&[track]).is_none(), "stale request opened a lease");
        lock(&controller)
            .send(Command::ChannelNote {
                channel,
                key: 60,
                velocity: 0.5,
                on: true,
                id: NoteId::live(),
            })
            .unwrap();
        processor.process(&mut output, 2);
        let peak = output
            .as_chunks::<2>()
            .0
            .iter()
            .fold([0.0f32; 2], |peak, frame| {
                [peak[0].max(frame[0].abs()), peak[1].max(frame[1].abs())]
            });
        assert!(peak[0] > peak[1] && peak[1] > 0.0);
        let engine_revision = lock(&controller).mix_revision();
        let reply = current(&[track, u32::MAX, track]).unwrap();
        assert_eq!(
            serde_json::to_value(reply).unwrap(),
            serde_json::json!({
                "generation": generation, "revision": revision,
                "mixRevision": engine_revision.to_string(), "frames": (BLOCK * 2).to_string(),
                "tracks": [{"track": track, "peak": peak, "clipFrame": null}]
            })
        );
        assert!(current(&[u32::MAX]).is_none());
        assert!(current(&[]).is_none());
        assert!(current(&[track]).is_none());
    }

    #[test]
    fn mixer_meter_bridge_rejects_replaced_projects_and_invalid_inputs() {
        let (session, controller, mut processor) = rig();
        let old_track = track(&session);
        let (old_generation, _) = lock(&session).mixer_stamp();
        lock(&session).new_project().unwrap();
        let track = track(&session);
        assert_eq!(track, old_track);
        let (generation, revision) = lock(&session).mixer_stamp();
        assert_ne!(generation, old_generation);
        let current =
            |tracks: &[u32]| poll(&session, &controller, generation, revision, tracks).unwrap();
        assert!(current(&[track]).is_none());
        processor.process(&mut [0.0; BLOCK * 2], 2);
        assert!(current(&[track]).is_some());
        assert!(
            poll(&session, &controller, old_generation, revision, &[])
                .unwrap()
                .is_none()
        );
        processor.process(&mut [0.0; BLOCK * 2], 2);
        assert_eq!(
            current(&[track]).unwrap().frames,
            (BLOCK * 2).to_string(),
            "stale cancel altered the current lease"
        );
        for (generation, revision, tracks) in [
            (MAX_SAFE_INTEGER + 1, revision, vec![track]),
            (generation, MAX_SAFE_INTEGER + 1, vec![track]),
            (generation, revision, vec![0]),
            (
                generation,
                revision,
                vec![track; MIXER_METER_MAX_TRACKS + 1],
            ),
        ] {
            assert!(poll(&session, &controller, generation, revision, &tracks).is_err());
        }
    }
}
