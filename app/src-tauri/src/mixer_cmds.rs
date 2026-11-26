//! The native mixer boundary. Session owns admission, publication and shared Undo.
use crate::{Audio, lock};
use grooviq_engine::{BLOCK, InstrumentKind, mixer as engine_mixer};
use grooviq_project::{
    mixer::{EFFECTS, Insert, Level, Limits, Mixer, Send, SourceKey, Tap, Track},
    mixer_edit::{CreatedId, IdRef, MixerAck, MixerEdit, MixerView},
    session::Session,
};
use serde::{Deserialize, Serialize, de};
use std::{fmt, time::Instant};
use tauri::State;

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const MAX_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerSnapshot {
    generation: u64,
    revision: u64,
    mixer: Mixer,
    // None means the binding must disable route choices.
    route_inputs: Option<Vec<RouteInput>>,
    sources: Vec<SourceChoice>,
    effects: &'static [&'static str],
    unsupported: UnsupportedControls,
}

#[derive(Debug, PartialEq, Serialize)]
struct RouteInput {
    track: u32,
    from: Vec<u32>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceChoice {
    source: SourceKey,
    name: String,
    instrument: Option<Instrument>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Instrument {
    Sampler,
    Synth,
}

#[derive(Debug, PartialEq, Serialize)]
struct UnsupportedControls {
    mute: &'static str,
    solo: &'static str,
    bypass: &'static str,
    detector: &'static str,
}

fn compile_route_inputs(mixer: &Mixer) -> Result<Vec<RouteInput>, engine_mixer::CompileError> {
    // Match the live topology budget. Descriptors carry no device or buffer preparation.
    let budget = engine_mixer::Budget {
        block_frames: BLOCK as u32,
        ..Default::default()
    };
    let compiled = engine_mixer::compile(&mixer.descriptors(), budget)?;
    let wire_id = |id: engine_mixer::MixerTrackId| {
        u32::try_from(id.0).map_err(|_| engine_mixer::CompileError::Overflow)
    };
    // The compiler emits tracks in ID order. Its inputs without a track are source assignments.
    compiled
        .tracks
        .into_iter()
        .map(|track| {
            let mut from = track
                .inputs
                .into_iter()
                .filter_map(|input| input.from)
                .map(wire_id)
                .collect::<Result<Vec<_>, _>>()?;
            from.sort_unstable();
            from.dedup();
            Ok(RouteInput {
                track: wire_id(track.track.id)?,
                from,
            })
        })
        .collect()
}

fn snapshot_with_view(session: &Session, view: MixerView) -> MixerSnapshot {
    let project = session.project();
    // These display labels come from legacy sources, outside the mixer's string budget.
    let label =
        |name: &str| name[..name.floor_char_boundary(Limits::DEFAULT.name_bytes)].to_owned();
    let sources = project
        .channels
        .iter()
        .map(|channel| SourceChoice {
            source: SourceKey::Channel(channel.id),
            name: label(&channel.name),
            instrument: Some(match channel.instrument {
                InstrumentKind::Sampler => Instrument::Sampler,
                InstrumentKind::Synth => Instrument::Synth,
            }),
        })
        .chain(project.arrangement.tracks.iter().map(|track| SourceChoice {
            source: SourceKey::AudioTrack(track.id),
            name: label(&track.name),
            instrument: None,
        }))
        .collect();
    let route_inputs = match compile_route_inputs(&view.mixer) {
        Ok(inputs) => Some(inputs),
        Err(error) => {
            eprintln!("Mixer route inputs unavailable: {error:?}");
            None
        }
    };
    MixerSnapshot {
        generation: view.generation,
        revision: view.revision,
        mixer: view.mixer,
        route_inputs,
        sources,
        effects: &EFFECTS,
        unsupported: UnsupportedControls {
            mute: "Mixer track mute is unavailable.",
            solo: "Mixer track solo is unavailable.",
            bypass: "Insert bypass is unavailable.",
            detector: "Editing detector routes is unavailable.",
        },
    }
}

fn snapshot(session: &Session) -> MixerSnapshot {
    snapshot_with_view(session, session.mixer_view())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    generation: u64,
    revision: u64,
    edit: Edit,
}

/// One stamped action per request. Create-and-assign expands into two domain operations.
#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Edit {
    CreateTrack {
        name: String,
        output: u32,
        source: Option<SourceKey>,
    },
    SetOutput {
        track: u32,
        output: u32,
    },
    AssignSource {
        source: SourceKey,
        destination: u32,
    },
    SetFader {
        track: u32,
        level: Option<Level>,
        pan: Option<f32>,
    },
    AddInsert {
        track: u32,
        index: usize,
        effect_id: String,
    },
    RemoveInsert {
        track: u32,
        insert: u32,
    },
    MoveInsert {
        track: u32,
        insert: u32,
        destination_track: Option<u32>,
        index: usize,
    },
    SetInsert {
        track: u32,
        insert: u32,
        #[serde(deserialize_with = "parameters")]
        params: Vec<f32>,
    },
    ApplyPreset {
        track: u32,
        insert: u32,
        preset: String,
    },
    AddSend {
        from: u32,
        to: u32,
        #[serde(default)]
        tap: Tap,
        gain: f32,
    },
    SetSend {
        from: u32,
        send: u32,
        to: Option<u32>,
        tap: Option<Tap>,
        gain: Option<f32>,
    },
    RemoveSend {
        from: u32,
        send: u32,
    },
}

fn parameters<'de, D: de::Deserializer<'de>>(de: D) -> Result<Vec<f32>, D::Error> {
    struct Bank;
    impl<'de> de::Visitor<'de> for Bank {
        type Value = Vec<f32>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a bounded array of insert parameters")
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<f32>, A::Error> {
            let max = EFFECTS
                .iter()
                .filter_map(|id| grooviq_dsp::param_spec(id))
                .map(<[_]>::len)
                .max()
                .unwrap_or(0);
            let mut values = Vec::new();
            while values.len() < max {
                let Some(value) = seq.next_element::<f32>()? else {
                    return Ok(values);
                };
                values.push(value);
            }
            if seq.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("Too many insert parameters"));
            }
            Ok(values)
        }
    }
    de.deserialize_seq(Bank)
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Status {
    Applied,
    Refused,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MixerEditReply {
    status: Status,
    state: MixerSnapshot,
    changed: bool,
    created_ids: Vec<Creation>,
    message: Option<String>,
}

#[derive(Serialize)]
struct Creation {
    kind: EntityKind,
    id: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum EntityKind {
    Track,
    Insert,
    Send,
}

impl From<CreatedId> for Creation {
    fn from(value: CreatedId) -> Self {
        let (kind, id) = match value {
            CreatedId::Track(id) => (EntityKind::Track, id),
            CreatedId::Insert(id) => (EntityKind::Insert, id),
            CreatedId::Send(id) => (EntityKind::Send, id),
        };
        Self { kind, id }
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum MixerInsertReply {
    Applied {
        detail: InsertDetail,
    },
    Refused {
        state: MixerSnapshot,
        message: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertDetail {
    generation: u64,
    revision: u64,
    track: u32,
    insert: u32,
    effect_id: String,
    active: bool,
    params: Vec<f32>,
    preset: Option<String>,
    factory_presets: &'static [&'static str],
}

fn check_stamp(view: &MixerView, generation: u64, revision: u64) -> Result<(), &'static str> {
    if generation > MAX_SAFE_INTEGER || revision > MAX_SAFE_INTEGER {
        return Err("Mixer stamp is outside the supported range.");
    }
    if generation != view.generation {
        return Err("The project was replaced. Reload the mixer.");
    }
    if revision != view.revision {
        return Err("The mixer changed. Reload the mixer.");
    }
    Ok(())
}

fn find_track(mixer: &Mixer, track: u32) -> Result<&Track, &'static str> {
    mixer
        .tracks
        .iter()
        .find(|candidate| candidate.id == track)
        .ok_or("The mixer track no longer exists.")
}

fn find_insert(mixer: &Mixer, track: u32, insert: u32) -> Result<&Insert, &'static str> {
    find_track(mixer, track)?
        .inserts
        .iter()
        .find(|candidate| candidate.id == insert)
        .ok_or("The insert is no longer on this mixer track.")
}

fn find_send(mixer: &Mixer, from: u32, id: u32) -> Result<&Send, &'static str> {
    mixer
        .sends
        .iter()
        .find(|send| send.id == id && send.from == from)
        .ok_or("The send is no longer on this mixer track.")
}

fn default_bank(effect: &str) -> Result<Vec<f32>, &'static str> {
    let spec = grooviq_dsp::param_spec(effect)
        .filter(|_| EFFECTS.contains(&effect))
        .ok_or("Unknown built-in mixer effect.")?;
    Ok(spec.iter().map(|&(_, _, default)| default).collect())
}

fn factory_bank(effect: &str, preset: &str) -> Result<Vec<f32>, &'static str> {
    let mut params = default_bank(effect)?;
    let assignments = grooviq_dsp::preset(effect, preset).ok_or("Unknown factory preset.")?;
    for (index, value) in assignments {
        let param = params
            .get_mut(index as usize)
            .ok_or("The factory preset contains an invalid parameter index.")?;
        *param = value;
    }
    Ok(params)
}

impl Edit {
    fn into_ops(self, mixer: &Mixer) -> Result<Vec<MixerEdit>, &'static str> {
        let id = IdRef::Id;
        let op = match self {
            Self::CreateTrack {
                name,
                output,
                source,
            } => {
                let create = MixerEdit::CreateTrack {
                    name,
                    output: id(output),
                };
                return Ok(if let Some(source) = source {
                    vec![
                        create,
                        MixerEdit::Assign {
                            source,
                            destination: IdRef::Created(0),
                        },
                    ]
                } else {
                    vec![create]
                });
            }
            Self::SetOutput { track, output } => MixerEdit::SetOutput {
                track: id(track),
                output: id(output),
            },
            Self::AssignSource {
                source,
                destination,
            } => MixerEdit::Assign {
                source,
                destination: id(destination),
            },
            Self::SetFader { track, level, pan } => {
                let current = find_track(mixer, track)?;
                MixerEdit::SetFader {
                    track: id(track),
                    level: level.unwrap_or(current.level),
                    pan: pan.unwrap_or(current.pan),
                }
            }
            Self::AddInsert {
                track,
                index,
                effect_id,
            } => MixerEdit::AddInsert {
                track: id(track),
                index,
                params: default_bank(&effect_id)?,
                device: effect_id,
                preset: None,
            },
            Self::RemoveInsert { track, insert } => {
                find_insert(mixer, track, insert)?;
                MixerEdit::RemoveInsert { insert: id(insert) }
            }
            Self::MoveInsert {
                track,
                insert,
                destination_track,
                index,
            } => {
                find_insert(mixer, track, insert)?;
                MixerEdit::MoveInsert {
                    insert: id(insert),
                    track: id(destination_track.unwrap_or(track)),
                    index,
                }
            }
            Self::SetInsert {
                track,
                insert,
                params,
            } => {
                let current = find_insert(mixer, track, insert)?;
                let preset = if params == current.params {
                    current.preset.clone()
                } else {
                    None
                };
                MixerEdit::SetInsert {
                    insert: id(insert),
                    params,
                    preset,
                }
            }
            Self::ApplyPreset {
                track,
                insert,
                preset,
            } => {
                let current = find_insert(mixer, track, insert)?;
                MixerEdit::SetInsert {
                    insert: id(insert),
                    params: factory_bank(&current.device, &preset)?,
                    preset: Some(preset),
                }
            }
            Self::AddSend {
                from,
                to,
                tap,
                gain,
            } => MixerEdit::CreateSend {
                from: id(from),
                to: id(to),
                tap,
                gain,
            },
            Self::SetSend {
                from,
                send,
                to,
                tap,
                gain,
            } => {
                let current = find_send(mixer, from, send)?;
                MixerEdit::SetSend {
                    send: id(send),
                    from: id(from),
                    to: id(to.unwrap_or(current.to)),
                    tap: tap.unwrap_or(current.tap),
                    gain: gain.unwrap_or(current.gain),
                }
            }
            Self::RemoveSend { from, send } => {
                find_send(mixer, from, send)?;
                MixerEdit::DeleteSend { send: id(send) }
            }
        };
        Ok(vec![op])
    }
}

fn edit_mixer(session: &mut Session, request_json: &str) -> MixerEditReply {
    let before = session.mixer_view();
    let result = (|| {
        // Bound the inner payload before Serde buffers the tagged edit's fields.
        if request_json.len() > MAX_REQUEST_BYTES {
            return Err("Mixer edit request is too large.".into());
        }
        let request: Request = serde_json::from_str(request_json)
            .map_err(|error| format!("Invalid mixer edit request: {error}"))?;
        check_stamp(&before, request.generation, request.revision)?;
        let ops = request.edit.into_ops(&before.mixer)?;
        session
            .edit_mixer(request.generation, request.revision, ops, Instant::now())
            .map_err(|error| error.to_string())
    })();
    match result {
        Ok(MixerAck {
            state,
            changed,
            created_ids,
        }) => MixerEditReply {
            status: Status::Applied,
            state: snapshot_with_view(session, state),
            changed,
            created_ids: created_ids.into_iter().map(|(_, id)| id.into()).collect(),
            message: None,
        },
        Err(message) => MixerEditReply {
            status: Status::Refused,
            state: snapshot_with_view(session, before),
            changed: false,
            created_ids: Vec::new(),
            message: Some(message),
        },
    }
}

fn insert_detail(
    session: &Session,
    generation: u64,
    revision: u64,
    track: u32,
    insert: u32,
) -> MixerInsertReply {
    let view = session.mixer_view();
    match check_stamp(&view, generation, revision)
        .and_then(|()| find_insert(&view.mixer, track, insert))
    {
        Ok(slot) => MixerInsertReply::Applied {
            detail: InsertDetail {
                generation: view.generation,
                revision: view.revision,
                track,
                insert,
                effect_id: slot.device.clone(),
                active: !slot.bypass,
                params: slot.params.clone(),
                preset: slot.preset.clone(),
                factory_presets: grooviq_dsp::presets(&slot.device),
            },
        },
        Err(message) => MixerInsertReply::Refused {
            state: snapshot_with_view(session, view),
            message: message.into(),
        },
    }
}

#[tauri::command]
pub fn mixer_view(audio: State<'_, Audio>) -> MixerSnapshot {
    snapshot(&lock(&audio.session))
}

#[tauri::command]
pub fn mixer_edit(audio: State<'_, Audio>, request_json: String) -> MixerEditReply {
    edit_mixer(&mut lock(&audio.session), &request_json)
}

#[tauri::command]
pub fn mixer_insert_detail(
    audio: State<'_, Audio>,
    generation: u64,
    revision: u64,
    track: u32,
    insert: u32,
) -> MixerInsertReply {
    insert_detail(&lock(&audio.session), generation, revision, track, insert)
}

#[cfg(test)]
mod tests {
    use super::*;
    use grooviq_engine::{AudioProcessor, EngineConfig};
    use grooviq_project::arrangement_edit::ArrangementEdit;
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    fn setup() -> (Session, AudioProcessor) {
        let (controller, processor) = grooviq_engine::engine(EngineConfig {
            sample_rate: 48_000.0,
        });
        (Session::new(Arc::new(Mutex::new(controller))), processor)
    }

    fn request(session: &Session, edit: Value) -> String {
        let view = session.mixer_view();
        json!({"generation": view.generation, "revision": view.revision, "edit": edit}).to_string()
    }

    fn apply(session: &mut Session, edit: Value) -> MixerEditReply {
        let request = request(session, edit);
        let reply = edit_mixer(session, &request);
        assert_eq!(reply.status, Status::Applied, "{:?}", reply.message);
        reply
    }

    fn track(session: &mut Session, name: &str) -> u32 {
        apply(
            session,
            json!({"kind":"createTrack", "name":name, "output":0}),
        )
        .created_ids[0]
            .id
    }

    fn insert(session: &mut Session, track: u32) -> u32 {
        apply(
            session,
            json!({"kind":"addInsert", "track":track, "index":0, "effectId":"drum_shaper"}),
        )
        .created_ids[0]
            .id
    }

    fn undo(session: &mut Session) {
        let stamp = session.mixer_view();
        session
            .step_project_history(stamp.generation, stamp.revision, true, Instant::now())
            .unwrap();
    }

    fn assert_refused(session: &mut Session, request: &str) {
        let before = snapshot(session);
        let history = session.history_usage();
        let dirty = session.view().dirty;
        let reply = edit_mixer(session, request);
        assert_eq!(reply.status, Status::Refused);
        assert!(!reply.changed);
        assert!(reply.created_ids.is_empty());
        assert_eq!(reply.state, before);
        assert_eq!(snapshot(session), before);
        assert_eq!(session.history_usage(), history);
        assert_eq!(session.view().dirty, dirty);
    }

    #[test]
    fn snapshot_projects_compiler_predecessors_in_track_id_order() {
        let (mut session, mut processor) = setup();
        assert_eq!(
            serde_json::to_value(snapshot(&session)).unwrap()["routeInputs"],
            json!([{"track": 0, "from": []}])
        );
        let detector = track(&mut session, "Detector");
        let zero_send = track(&mut session, "Zero send");
        let main = track(&mut session, "Main");
        let bus = track(&mut session, "Bus");
        session.add_synth_channel("Init", Instant::now()).unwrap();
        let source = SourceKey::Channel(session.project().channels[0].id);
        processor.process(&mut [0.0; 128], 2);
        let before = session.mixer_view();
        let ack = session
            .edit_mixer(
                before.generation,
                before.revision,
                vec![
                    MixerEdit::SetOutput {
                        track: IdRef::Id(main),
                        output: IdRef::Id(bus),
                    },
                    MixerEdit::CreateSend {
                        from: IdRef::Id(zero_send),
                        to: IdRef::Id(bus),
                        tap: Tap::Post,
                        gain: 0.0,
                    },
                    MixerEdit::CreateSend {
                        from: IdRef::Id(detector),
                        to: IdRef::Id(0),
                        tap: Tap::Pre,
                        gain: 0.25,
                    },
                    MixerEdit::AddInsert {
                        track: IdRef::Id(bus),
                        index: 0,
                        device: "comp".into(),
                        params: default_bank("comp").unwrap(),
                        preset: None,
                    },
                    MixerEdit::SetSidechain {
                        previous: None,
                        from: IdRef::Id(detector),
                        to: IdRef::Created(3),
                        tap: Tap::Pre,
                    },
                    MixerEdit::Assign {
                        source,
                        destination: IdRef::Id(zero_send),
                    },
                ],
                Instant::now(),
            )
            .unwrap();
        let state = snapshot(&session);
        assert_eq!(
            (state.generation, state.revision),
            (ack.state.generation, ack.state.revision)
        );
        assert_eq!(state.mixer.destination(source), zero_send);
        let expected = json!([
            {"track": 0, "from": [detector, zero_send, bus]},
            {"track": detector, "from": []},
            {"track": zero_send, "from": []},
            {"track": main, "from": []},
            {"track": bus, "from": [detector, zero_send, main]},
        ]);
        assert_eq!(
            serde_json::to_value(state).unwrap()["routeInputs"],
            expected
        );

        let mut reordered = session.mixer_view();
        reordered.mixer.tracks.reverse();
        reordered.mixer.sends.reverse();
        assert_eq!(
            serde_json::to_value(snapshot_with_view(&session, reordered)).unwrap()["routeInputs"],
            expected
        );
    }

    #[test]
    fn snapshot_marks_unavailable_routes_explicitly_when_compilation_fails() {
        let (session, _processor) = setup();
        let mut view = session.mixer_view();
        view.mixer.tracks.clear();
        let packet = serde_json::to_value(snapshot_with_view(&session, view)).unwrap();
        assert_eq!(packet.get("routeInputs"), Some(&Value::Null));
    }

    #[test]
    fn stale_generation_cannot_edit_same_track_and_revision_in_replacement() {
        let (mut session, mut processor) = setup();
        let old_track = track(&mut session, "Old");
        let old_stamp = session.mixer_view();
        let stale = request(
            &session,
            json!({"kind":"setFader","track":old_track,"pan":0.5}),
        );
        processor.process(&mut [0.0; 128], 2);
        session.new_project().unwrap();
        processor.process(&mut [0.0; 128], 2);
        let new_track = track(&mut session, "New");
        assert_eq!(new_track, old_track);
        assert_eq!(session.mixer_view().revision, old_stamp.revision);
        assert_ne!(session.mixer_view().generation, old_stamp.generation);
        assert_refused(&mut session, &stale);
    }

    #[test]
    fn same_generation_stale_revision_cannot_edit_existing_track() {
        let (mut session, _processor) = setup();
        let bus = track(&mut session, "Bus");
        let stale = request(&session, json!({"kind":"setFader","track":bus,"pan":0.5}));
        apply(
            &mut session,
            json!({"kind":"setFader","track":bus,"pan":-0.5}),
        );
        assert_refused(&mut session, &stale);
    }

    #[test]
    fn a_real_slot_on_another_track_cannot_be_opened() {
        let (mut session, _processor) = setup();
        let owner = track(&mut session, "Owner");
        let other = track(&mut session, "Other");
        let slot = insert(&mut session, owner);
        let stamp = session.mixer_view();
        assert!(matches!(
            insert_detail(&session, stamp.generation, stamp.revision, other, slot),
            MixerInsertReply::Refused { .. }
        ));
    }

    #[test]
    fn a_real_slot_on_another_track_cannot_be_changed() {
        let (mut session, _processor) = setup();
        let owner = track(&mut session, "Owner");
        let other = track(&mut session, "Other");
        let slot = insert(&mut session, owner);
        let wrong = [
            json!({"kind":"setInsert","track":other,"insert":slot,"params":[1,2,0,1,0,1,0]}),
            json!({"kind":"applyPreset","track":other,"insert":slot,"preset":"Kick Punch"}),
            json!({"kind":"moveInsert","track":other,"insert":slot,"index":0}),
            json!({"kind":"removeInsert","track":other,"insert":slot}),
        ];
        for edit in wrong {
            let request = request(&session, edit);
            assert_refused(&mut session, &request);
        }
    }

    #[test]
    fn zero_gain_send_closes_a_cycle_and_preserves_history_on_refusal() {
        let (mut session, _processor) = setup();
        let a = track(&mut session, "A");
        let b = track(&mut session, "B");
        apply(
            &mut session,
            json!({"kind":"setOutput","track":b,"output":a}),
        );
        let cyclic = request(&session, json!({"kind":"addSend","from":a,"to":b,"gain":0}));
        assert_refused(&mut session, &cyclic);
        undo(&mut session);
        assert_eq!(
            find_track(&session.project().mixer, b).unwrap().output,
            Some(0)
        );
    }

    #[test]
    fn preset_replaces_all_values_and_label_with_one_undo_entry() {
        let (mut session, mut processor) = setup();
        let slot = insert(&mut session, 0);
        let custom = [3.0, 4.0, 2.0, 2.0, 0.8, 0.4, 7.0];
        apply(
            &mut session,
            json!({"kind":"setInsert","track":0,"insert":slot,"params":custom}),
        );
        processor.process(&mut [0.0; 128], 2);
        let before = session.mixer_view();
        let ack = apply(
            &mut session,
            json!({"kind":"applyPreset","track":0,"insert":slot,"preset":"Kick Punch"}),
        );
        assert_eq!(ack.state.revision, before.revision + 1);
        let current = session.mixer_view();
        let MixerInsertReply::Applied { detail } =
            insert_detail(&session, current.generation, current.revision, 0, slot)
        else {
            panic!("expected current insert detail");
        };
        assert_eq!(
            (detail.generation, detail.revision),
            (current.generation, current.revision)
        );
        assert!(detail.active);
        assert_eq!(detail.effect_id, "drum_shaper");
        assert_eq!(detail.params, [6.0, -3.0, 0.0, 0.0, 0.1, 1.0, -2.0]);
        assert_eq!(detail.preset.as_deref(), Some("Kick Punch"));
        assert_eq!(ack.state.mixer.tracks[0].inserts[0].params, detail.params);
        let history = session.history_usage();
        let no_op = apply(
            &mut session,
            json!({"kind":"setInsert","track":0,"insert":slot,"params":detail.params}),
        );
        assert!(!no_op.changed);
        assert_eq!(no_op.state, ack.state);
        assert_eq!(session.history_usage(), history);
        processor.process(&mut [0.0; 128], 2);
        undo(&mut session);
        assert_eq!(session.mixer_view().mixer, before.mixer);
        assert_eq!(
            session.mixer_view().mixer.tracks[0].inserts[0].params,
            custom
        );
    }

    #[test]
    fn create_and_assign_publishes_once_and_undo_leaves_complete_sparse_sources() {
        let (mut session, mut processor) = setup();
        session.add_synth_channel("Init", Instant::now()).unwrap();
        let channel = session.project().channels[0].id;
        let stamp = session.mixer_view();
        session
            .edit_arrangement(
                stamp.generation,
                stamp.revision,
                vec![ArrangementEdit::CreateTrack {
                    name: "Audio".into(),
                    color: 0,
                }],
                Instant::now(),
            )
            .unwrap();
        let audio = session.project().arrangement.tracks[0].id;
        processor.process(&mut [0.0; 128], 2);
        let before = snapshot(&session);
        assert_eq!(before.sources.len(), 2);
        assert_eq!(before.sources[0].source, SourceKey::Channel(channel));
        assert_eq!(before.sources[1].source, SourceKey::AudioTrack(audio));
        assert!(before.mixer.sources.is_empty());
        let unknown = request(
            &session,
            json!({"kind":"createTrack","name":"Refused","output":0,
            "source":{"channel":u32::MAX}}),
        );
        assert_refused(&mut session, &unknown);
        let reply = apply(
            &mut session,
            json!({"kind":"createTrack","name":"Bus","output":0,
            "source":{"audioTrack":audio}}),
        );
        let bus = reply.created_ids[0].id;
        assert_eq!(reply.state.revision, before.revision + 1);
        assert_eq!(
            reply.state.mixer.destination(SourceKey::AudioTrack(audio)),
            bus
        );
        assert_eq!(
            reply.state.mixer.destination(SourceKey::Channel(channel)),
            0
        );
        undo(&mut session);
        let after = snapshot(&session);
        assert_eq!(after.mixer.tracks, before.mixer.tracks);
        assert_eq!(after.mixer.sources, before.mixer.sources);
        assert_eq!(after.sources, before.sources);
    }

    #[test]
    fn partial_controls_keep_paired_values_and_no_op_preserves_history() {
        let (mut session, _processor) = setup();
        let bus = track(&mut session, "Bus");
        apply(
            &mut session,
            json!({"kind":"setFader","track":bus,"level":{"db":-3},"pan":-0.25}),
        );
        apply(
            &mut session,
            json!({"kind":"setFader","track":bus,"pan":0.5}),
        );
        let send = apply(
            &mut session,
            json!({"kind":"addSend","from":bus,"to":0,"gain":0.25}),
        )
        .created_ids[0]
            .id;
        apply(
            &mut session,
            json!({"kind":"setSend","from":bus,"send":send,"tap":"pre"}),
        );
        let next = apply(
            &mut session,
            json!({"kind":"setSend","from":bus,"send":send,"gain":0.5}),
        );
        let fader = find_track(&next.state.mixer, bus).unwrap();
        assert_eq!((fader.level, fader.pan), (Level::Db(-3.0), 0.5));
        assert_eq!(
            (
                next.state.mixer.sends[0].gain,
                next.state.mixer.sends[0].tap
            ),
            (0.5, Tap::Pre)
        );
        let history = session.history_usage();
        let no_op = apply(
            &mut session,
            json!({"kind":"setSend","from":bus,"send":send,"gain":0.5}),
        );
        assert!(!no_op.changed);
        assert_eq!(no_op.state, next.state);
        assert_eq!(session.history_usage(), history);
    }

    #[test]
    fn insert_move_keeps_identity_and_stale_detail_refuses_after_removal() {
        let (mut session, _processor) = setup();
        let bus = track(&mut session, "Bus");
        let slot = insert(&mut session, bus);
        let move_reply = apply(
            &mut session,
            json!({"kind":"moveInsert","track":bus,"insert":slot,"destinationTrack":0,"index":0}),
        );
        assert!(
            find_track(&move_reply.state.mixer, bus)
                .unwrap()
                .inserts
                .is_empty()
        );
        assert_eq!(
            find_insert(&move_reply.state.mixer, 0, slot).unwrap().id,
            slot
        );
        let removed = apply(
            &mut session,
            json!({"kind":"removeInsert","track":0,"insert":slot}),
        );
        let reply = insert_detail(
            &session,
            removed.state.generation,
            move_reply.state.revision,
            0,
            slot,
        );
        let MixerInsertReply::Refused { state, .. } = reply else {
            panic!("a removed insert has no editor detail");
        };
        assert_eq!(state, removed.state);
    }

    #[test]
    fn malformed_or_oversized_requests_and_incomplete_banks_preserve_the_session() {
        let (mut session, _processor) = setup();
        let slot = insert(&mut session, 0);
        for request in [
            "{".into(),
            " ".repeat(MAX_REQUEST_BYTES + 1),
            request(
                &session,
                json!({"kind":"setInsert","track":0,"insert":slot,"params":[1]}),
            ),
            request(
                &session,
                json!({"kind":"setInsert","track":0,"insert":slot,"params":vec![0;256]}),
            ),
            request(
                &session,
                json!({"kind":"applyPreset","track":0,"insert":slot,"preset":"Unknown"}),
            ),
            request(
                &session,
                json!({"kind":"setInsertActive","track":0,"insert":slot,"active":false}),
            ),
        ] {
            assert_refused(&mut session, &request);
        }
    }
}
