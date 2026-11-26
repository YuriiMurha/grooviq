//! Reduced workspace parity contract. Generated data, public Session APIs, offline only.
//! Compare wire traces with m3-workspace.e2e.ts using a per-domain bijection of opaque
//! stamps/tokens: first occurrence -> ordinal; repeated values MUST keep their ordinal,
//! distinct values MUST stay distinct; None/Some and refusal-preserved identities stay exact.
//! Never erase identities, revisions, targets, optional trim entries, or selection from a trace.
use grooviq_dsp::sampler::SampleBuf;
use grooviq_engine::{AudioProcessor, Command, Controller, EngineConfig, engine};
use grooviq_project::{
    DecodedSamples, Loaded, PlayMode, SampleFile,
    arrangement_edit::HistoryEffect,
    pattern_edit::PatternEdit,
    session::{Edit, Session},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

struct Rig {
    s: Session,
    c: Arc<Mutex<Controller>>,
    p: AudioProcessor,
    channels: [u32; 2],
    other: u32,
}
fn wav() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend(40u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(48000u32.to_le_bytes());
    bytes.extend(96000u32.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(4u32.to_le_bytes());
    bytes.extend(100i16.to_le_bytes());
    bytes.extend((-100i16).to_le_bytes());
    bytes
}
fn sample() -> Arc<SampleBuf> {
    Arc::new(SampleBuf {
        rate: 48000.0,
        l: vec![0.1; 2],
        r: vec![0.1; 2],
    })
}
impl Rig {
    fn drain(&mut self) {
        self.p.process(&mut [0.0; 128], 2);
    }
    fn apply(&mut self, edit: Edit) {
        self.s.apply(edit, Instant::now()).unwrap();
        self.drain();
    }
    fn select(&mut self, pattern: u32) {
        let v = self.s.patterns();
        self.s
            .edit_patterns(
                v.generation,
                v.revision,
                PatternEdit::Select { pattern },
                Instant::now(),
            )
            .unwrap();
        self.drain();
    }
    fn step(&mut self, undo: bool) -> HistoryEffect {
        let v = self.s.arrangement_view();
        let ack = self
            .s
            .step_project_history(v.generation, v.revision, undo, Instant::now())
            .unwrap();
        self.drain();
        ack.effect
    }
    fn state(&self) -> Value {
        // Runtime revisions are intentionally absent from persisted Pattern JSON.
        let patterns = self
            .s
            .project()
            .patterns
            .iter()
            .map(|pattern| {
                json!({ "id": pattern.id, "name": pattern.name, "steps": pattern.steps,
                    "swing": pattern.swing, "notes": pattern.notes,
                    "nextNoteId": pattern.next_note_id, "revision": pattern.revision })
            })
            .collect::<Vec<_>>();
        json!({ "generation": self.s.generation(), "arrangement": self.s.arrangement_view(),
            "selected": self.s.patterns().selected, "patterns": patterns,
            "dirty": self.s.view().dirty, "playMode": self.s.project().play_mode,
            "storedTrim": self.s.project().channels.iter().map(|channel| json!({ "channel": channel.id,
                "start": channel.sampler.get("start").map(|value| value.to_bits()),
                "end": channel.sampler.get("end").map(|value| value.to_bits()) })).collect::<Vec<_>>(),
            "trim": self.channels.iter().map(|id| (*id, self.s.sampler_trim_view(*id).ok())).collect::<Vec<_>>() })
    }
    fn reset(&mut self, channel: u32) {
        let v = self.s.sampler_trim_view(channel).unwrap();
        self.s
            .sampler_trim_reset(channel, &v.stamp, Instant::now())
            .unwrap();
        self.drain();
    }
}
fn rig() -> Rig {
    let (controller, processor) = engine(EngineConfig {
        sample_rate: 48000.0,
    });
    let c = Arc::new(Mutex::new(controller));
    let mut s = Session::new(c.clone());
    let mut ids = vec![];
    for index in 0..2 {
        let name = format!("{}.wav", ["A", "B"][index]);
        let path = format!("/qa/sounds/{name}");
        let v = s
            .add_channel(
                Path::new(&path),
                SampleFile {
                    name: name.clone(),
                    bytes: Arc::new(wav()),
                },
                sample(),
                Instant::now(),
            )
            .unwrap();
        ids.push(v.channels[index].id);
    }
    let other = s.project().next_id;
    let mut r = Rig {
        s,
        c,
        p: processor,
        channels: [ids[0], ids[1]],
        other,
    };
    r.drain();
    r.apply(Edit::SetInstrumentParam {
        channel: r.channels[1],
        param: "start".into(),
        value: 0.6,
    });
    r.apply(Edit::SetInstrumentParam {
        channel: r.channels[1],
        param: "end".into(),
        value: 0.4,
    });
    let mut project = r.s.project().clone();
    project.name = "QA beat".into();
    project.bpm = 120.0;
    let created = grooviq_project::pattern_edit::apply(
        &mut project,
        PatternEdit::Create { name: "B".into() },
    )
    .unwrap();
    assert_eq!(created, other);
    let loaded = Loaded {
        project,
        samples: r
            .channels
            .iter()
            .map(|id| {
                (
                    *id,
                    SampleFile {
                        name: "generated.wav".into(),
                        bytes: Arc::new(wav()),
                    },
                )
            })
            .collect(),
        warnings: vec![],
    };
    let decoded = DecodedSamples {
        samples: r.channels.iter().map(|id| (*id, sample())).collect(),
        warnings: vec![],
        missing: BTreeSet::new(),
    };
    r.s.open(Path::new("generated.grooviq"), loaded, decoded)
        .unwrap();
    r.drain();
    r
}

fn step_trace(r: &mut Rig, undo: bool, observations: &mut Vec<Value>) -> HistoryEffect {
    let stamp = r.s.arrangement_view();
    let ack =
        r.s.step_project_history(stamp.generation, stamp.revision, undo, Instant::now())
            .unwrap();
    observations.push(json!({ "reply": &ack, "state": r.state() }));
    r.drain();
    ack.effect
}

#[test]
fn project_menu_availability_includes_inactive_notes_and_trim_without_changing_legacy_flags() {
    let mut r = rig();
    let empty = r.s.view();
    assert!(!empty.project_can_undo && !empty.project_can_redo);
    r.select(r.other);
    r.apply(Edit::ToggleStep {
        channel: r.channels[0],
        step: 2,
    });
    r.select(0);
    let view = r.s.view();
    assert!(!view.can_undo && !view.can_redo);
    assert!(view.project_can_undo && !view.project_can_redo);
    assert!(matches!(r.step(true), HistoryEffect::Notes { pattern } if pattern == r.other));
    let view = r.s.view();
    assert!(!view.can_undo && !view.can_redo);
    assert!(!view.project_can_undo && view.project_can_redo);
    assert_eq!(r.s.patterns().selected, 0);
    r.reset(r.channels[1]);
    let view = r.s.view();
    assert!(!view.can_undo && !view.can_redo);
    assert!(view.project_can_undo && !view.project_can_redo);
    assert!(
        matches!(r.step(true), HistoryEffect::SamplerTrim { channel } if channel == r.channels[1])
    );
    let view = r.s.view();
    assert!(!view.project_can_undo && view.project_can_redo);
    assert_eq!(
        r.s.project().channels[1].sampler["start"].to_bits(),
        0.6f32.to_bits()
    );
    assert!(matches!(r.step(false), HistoryEffect::SamplerTrim { .. }));
    assert!(r.s.view().project_can_undo);
}

#[test]
fn selected_pattern_history_and_global_history_keep_distinct_native_targets() {
    let mut r = rig();
    let channel = r.channels[0];
    r.apply(Edit::ToggleStep { channel, step: 0 });
    r.select(r.other);
    r.apply(Edit::ToggleStep { channel, step: 2 });
    r.select(0);
    let original_a =
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == 0)
            .unwrap()
            .clone();
    let original_b =
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == r.other)
            .unwrap()
            .clone();
    let selected = r.s.pattern_stamp();
    let before = r.state();
    let wrong = grooviq_project::session::PatternStamp {
        pattern_id: r.other,
        revision: original_b.revision,
        ..selected
    };
    assert!(r.s.history_stamped(wrong, true, Instant::now()).is_err());
    assert_eq!(r.state(), before);
    let ack =
        r.s.history_stamped(selected, true, Instant::now())
            .unwrap()
            .unwrap();
    r.drain();
    assert_eq!(ack.view.pattern.id, 0);
    assert!(
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == 0)
            .unwrap()
            .notes
            .is_empty()
    );
    assert_eq!(
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == r.other)
            .unwrap(),
        &original_b
    );
    let stamp = r.s.pattern_stamp();
    r.s.history_stamped(stamp, false, Instant::now())
        .unwrap()
        .unwrap();
    r.drain();
    let restored_a =
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == 0)
            .unwrap()
            .clone();
    assert_eq!(restored_a.notes, original_a.notes);
    assert_eq!(
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == r.other)
            .unwrap(),
        &original_b
    );
    assert!(matches!(r.step(true), HistoryEffect::Notes { pattern } if pattern == r.other));
    assert_eq!(r.s.patterns().selected, 0);
    assert_eq!(
        r.s.project().patterns.iter().find(|p| p.id == 0).unwrap(),
        &restored_a
    );
    assert!(
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == r.other)
            .unwrap()
            .notes
            .is_empty()
    );
    assert!(matches!(r.step(false), HistoryEffect::Notes { pattern } if pattern == r.other));
    assert_eq!(
        r.s.project()
            .patterns
            .iter()
            .find(|p| p.id == r.other)
            .unwrap()
            .notes,
        original_b.notes
    );
    assert!(r.s.view().dirty);
    let stale = grooviq_project::session::PatternStamp {
        generation: r.s.generation() + 1,
        ..r.s.pattern_stamp()
    };
    let before = r.state();
    assert!(r.s.history_stamped(stale, true, Instant::now()).is_err());
    assert_eq!(r.state(), before);
}

#[test]
fn notes_a_trim_b_notes_c_global_inverse_order_and_inactive_target() {
    let mut r = rig();
    let [a, b] = r.channels;
    let mut observations = vec![];
    r.apply(Edit::ToggleStep {
        channel: a,
        step: 0,
    });
    r.reset(b);
    r.select(r.other);
    r.apply(Edit::ToggleStep {
        channel: a,
        step: 2,
    });
    r.select(0);
    let selected_revision = r.s.view().notes_revision;
    assert!(
        matches!(step_trace(&mut r,true,&mut observations), HistoryEffect::Notes { pattern } if pattern == r.other)
    );
    assert_eq!(r.s.patterns().selected, 0);
    assert_eq!(r.s.view().notes_revision, selected_revision);
    assert!(
        matches!(step_trace(&mut r,true,&mut observations), HistoryEffect::SamplerTrim { channel } if channel == b)
    );
    assert_eq!(
        r.s.project().channels[1].sampler["start"].to_bits(),
        0.6f32.to_bits()
    );
    assert!(matches!(
        step_trace(&mut r, true, &mut observations),
        HistoryEffect::Notes { pattern: 0 }
    ));
    assert!(r.s.project().patterns.iter().all(|p| p.notes.is_empty()));
    assert!(matches!(
        step_trace(&mut r, false, &mut observations),
        HistoryEffect::Notes { pattern: 0 }
    ));
    assert!(
        matches!(step_trace(&mut r,false,&mut observations), HistoryEffect::SamplerTrim { channel } if channel == b)
    );
    assert!(
        matches!(step_trace(&mut r,false,&mut observations), HistoryEffect::Notes { pattern } if pattern == r.other)
    );
    assert_eq!(
        r.s.project()
            .patterns
            .iter()
            .map(|p| p.notes.len())
            .sum::<usize>(),
        2
    );
    assert_eq!(observations.len(), 6);
    eprintln!("m3-workspace-parity-trace:{}", json!(observations));
}
#[test]
fn empty_noop_read_and_clean_save_do_not_advance_arrangement_stamp() {
    let mut r = rig();
    let stamp = r.s.arrangement_view().revision;
    assert!(matches!(r.step(true), HistoryEffect::Empty));
    let v = r.s.sampler_trim_view(r.channels[0]).unwrap();
    assert!(
        !r.s.sampler_trim_reset(r.channels[0], &v.stamp, Instant::now())
            .unwrap()
            .changed
    );
    r.s.set_play_mode_in(r.s.generation(), stamp, PlayMode::Pattern, Instant::now())
        .unwrap();
    assert_eq!(r.s.arrangement_view().revision, stamp);
    let dir = tempfile::tempdir().unwrap();
    r.s.save(
        Some(&dir.path().join("generated.grooviq")),
        &dir.path().join("backups"),
    )
    .unwrap();
    assert!(!r.s.view().dirty);
    assert_eq!(r.s.arrangement_view().revision, stamp);
    assert_eq!(r.s.sampler_trim_view(r.channels[0]).unwrap().stamp, v.stamp);
}
#[test]
fn stale_generation_and_writer_admission_refusal_preserve_every_inverse_and_capability() {
    let mut r = rig();
    r.reset(r.channels[1]);
    let before = r.state();
    let v = r.s.arrangement_view();
    assert!(
        r.s.step_project_history(v.generation + 1, v.revision, true, Instant::now())
            .is_err()
    );
    assert!(
        r.s.step_project_history(v.generation, v.revision + 1, true, Instant::now())
            .is_err()
    );
    assert_eq!(r.state(), before);
    r.s.set_precommit_validator(Some(Arc::new(|_| Err("generated writer refusal".into()))));
    assert!(
        r.s.step_project_history(v.generation, v.revision, true, Instant::now())
            .is_err()
    );
    assert_eq!(r.state(), before);
    r.s.set_precommit_validator(None);
    assert!(matches!(r.step(true), HistoryEffect::SamplerTrim { .. }));
}
#[test]
fn dedicated_undo_and_global_redo_restore_trim_values_and_refresh_stamps() {
    let mut r = rig();
    let b = r.channels[1];
    let before = r.s.project().channels[1].sampler.clone();
    r.reset(b);
    let reset = r.s.sampler_trim_view(b).unwrap();
    r.s.sampler_trim_undo(
        b,
        &reset.stamp,
        reset.undo.as_ref().unwrap(),
        Instant::now(),
    )
    .unwrap();
    r.drain();
    let undone = r.s.sampler_trim_view(b).unwrap();
    assert_ne!(undone.stamp, reset.stamp);
    assert!(undone.undo.is_none());
    assert_eq!(r.s.project().channels[1].sampler, before);
    assert!(matches!(r.step(false), HistoryEffect::SamplerTrim { channel } if channel == b));
    let redone = r.s.sampler_trim_view(b).unwrap();
    assert_ne!(redone.stamp, undone.stamp);
    assert_eq!(redone.undo, reset.undo);
    assert!(
        r.s.sampler_trim_undo(
            b,
            &reset.stamp,
            reset.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
}

#[test]
fn noop_trim_reset_and_empty_undo_preserve_replacement_project() {
    let mut r = rig();
    let b = r.channels[1];
    r.reset(b);
    assert!(r.s.view().project_can_undo);

    let mut project = r.s.project().clone();
    project.channels[1].sampler.remove("start");
    let loaded = Loaded {
        project,
        samples: HashMap::new(),
        warnings: vec![],
    };
    r.s.open(
        Path::new("generated.grooviq"),
        loaded,
        DecodedSamples {
            samples: HashMap::from([(b, sample())]),
            warnings: vec![],
            missing: BTreeSet::new(),
        },
    )
    .unwrap();
    r.drain();
    let before = r.s.project().clone();
    assert!(!before.channels[1].sampler.contains_key("start"));
    let current = r.s.sampler_trim_view(b).unwrap();
    let reset =
        r.s.sampler_trim_reset(b, &current.stamp, Instant::now())
            .unwrap();
    r.drain();
    assert!(!reset.changed);
    assert_eq!(r.s.project(), &before);
    assert!(matches!(r.step(true), HistoryEffect::Empty));
    assert_eq!(r.s.history_usage().0, 0);
    assert_eq!(r.s.project(), &before);
}
#[test]
fn ordinary_trim_and_relink_retire_shared_stacks_but_observation_does_not() {
    let mut r = rig();
    let b = r.channels[1];
    r.reset(b);
    let old = r.s.sampler_trim_view(b).unwrap();
    let before = r.state();
    for _ in 0..3 {
        r.s.sampler_trim_view(b).unwrap();
    }
    assert_eq!(r.state(), before);
    r.apply(Edit::SetInstrumentParam {
        channel: b,
        param: "start".into(),
        value: 0.2,
    });
    r.apply(Edit::SetInstrumentParam {
        channel: b,
        param: "start".into(),
        value: 0.0,
    });
    assert!(matches!(r.step(true), HistoryEffect::Empty));
    assert!(
        r.s.sampler_trim_undo(b, &old.stamp, old.undo.as_ref().unwrap(), Instant::now())
            .is_err()
    );
    r.apply(Edit::SetInstrumentParam {
        channel: b,
        param: "start".into(),
        value: 0.3,
    });
    r.reset(b);
    let capability = r.s.sampler_trim_view(b).unwrap();
    r.s.set_channel_sample(
        b,
        Path::new("generated-1.wav"),
        SampleFile {
            name: "generated-1.wav".into(),
            bytes: Arc::new(wav()),
        },
        sample(),
        Instant::now(),
    )
    .unwrap();
    r.drain();
    assert_ne!(r.s.sampler_trim_view(b).unwrap().stamp, capability.stamp);
    assert!(matches!(r.step(true), HistoryEffect::Empty));
}
#[test]
fn mode_roundtrip_new_open_and_failed_admission_do_not_play_or_rebind_held_results() {
    let mut r = rig();
    let v = r.s.arrangement_view();
    r.s.set_precommit_validator(Some(Arc::new(|_| Err("mode admission".into()))));
    let before = r.state();
    assert!(
        r.s.set_play_mode_in(v.generation, v.revision, PlayMode::Song, Instant::now())
            .is_err()
    );
    assert_eq!(r.state(), before);
    r.s.set_precommit_validator(None);
    r.s.set_play_mode_in(v.generation, v.revision, PlayMode::Song, Instant::now())
        .unwrap();
    r.drain();
    assert!(!r.c.lock().unwrap().live().playing);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("roundtrip.grooviq");
    r.s.save(Some(&path), &dir.path().join("backups")).unwrap();
    let stamp = r.s.arrangement_view().revision;
    let loaded = grooviq_project::load(&path).unwrap();
    let decoded = grooviq_project::decode_samples(&loaded);
    r.s.open(&path, loaded, decoded).unwrap();
    r.drain();
    assert_eq!(r.s.project().play_mode, PlayMode::Song);
    assert_eq!(r.s.arrangement_view().revision, 0);
    assert!(!r.c.lock().unwrap().live().playing);
    assert!(stamp > 0);
    // Captured reply is a value from one execution, not a request to execute after release.
    r.apply(Edit::ToggleStep {
        channel: r.channels[0],
        step: 0,
    });
    let v = r.s.arrangement_view();
    let captured =
        r.s.step_project_history(v.generation, v.revision, true, Instant::now())
            .unwrap();
    r.drain();
    r.s.new_project().unwrap();
    r.drain();
    let fresh = r.s.generation();
    assert_eq!(captured.state.generation, v.generation);
    assert_ne!(v.generation, fresh);
    assert_ne!(captured.state.generation, fresh);
    let stale =
        r.s.step_project_history(v.generation, v.revision, true, Instant::now())
            .unwrap_err();
    assert_eq!(stale.state.generation, fresh);
    assert_eq!(stale.message, "Project replaced; reload arrangement");
    assert!(stale.reload);
    assert!(r.s.project().channels.is_empty());
    assert!(!r.c.lock().unwrap().live().playing);
}
#[test]
fn failed_open_and_reused_channel_ids_cannot_accept_old_trim_capabilities() {
    let mut r = rig();
    let id = r.channels[0];
    r.apply(Edit::SetInstrumentParam {
        channel: id,
        param: "start".into(),
        value: 0.2,
    });
    r.reset(id);
    let capability = r.s.sampler_trim_view(id).unwrap();
    let before = r.state();
    // Real engine admission failure at the public Open boundary.
    while r.c.lock().unwrap().send(Command::Stop).is_ok() {}
    let loaded = Loaded {
        project: r.s.project().clone(),
        samples: HashMap::new(),
        warnings: vec![],
    };
    let result = r.s.open(
        Path::new("refused.grooviq"),
        loaded,
        DecodedSamples {
            samples: HashMap::new(),
            warnings: vec![],
            missing: BTreeSet::new(),
        },
    );
    assert!(result.is_err());
    assert_eq!(r.state(), before);
    r.drain();
    r.s.new_project().unwrap();
    r.drain();
    let view =
        r.s.add_channel(
            Path::new("reuse.wav"),
            SampleFile {
                name: "reuse.wav".into(),
                bytes: Arc::new(wav()),
            },
            sample(),
            Instant::now(),
        )
        .unwrap();
    r.drain();
    assert_eq!(view.channels[0].id, id);
    assert!(
        r.s.sampler_trim_undo(
            id,
            &capability.stamp,
            capability.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
}
#[test]
fn public_picker_create_duplicate_rename_and_failed_admission_are_canonical() {
    let mut r = rig();
    let base = r.s.patterns();
    let created =
        r.s.edit_patterns(
            base.generation,
            base.revision,
            PatternEdit::Create {
                name: "New pattern".into(),
            },
            Instant::now(),
        )
        .unwrap();
    r.drain();
    assert_eq!(created.selected, base.selected);
    let id = created
        .patterns
        .iter()
        .find(|pattern| pattern.name == "New pattern")
        .unwrap()
        .id;
    r.select(id);
    let selected = r.s.patterns();
    let renamed =
        r.s.edit_patterns(
            selected.generation,
            selected.revision,
            PatternEdit::Rename {
                pattern: id,
                name: "Melody".into(),
            },
            Instant::now(),
        )
        .unwrap();
    r.drain();
    r.s.set_precommit_validator(Some(Arc::new(|_| Err("generated busy".into()))));
    let before = r.state();
    assert!(
        r.s.edit_patterns(
            renamed.generation,
            renamed.revision,
            PatternEdit::Duplicate { pattern: id },
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(r.state(), before);
    r.s.set_precommit_validator(None);
    let duplicated =
        r.s.edit_patterns(
            renamed.generation,
            renamed.revision,
            PatternEdit::Duplicate { pattern: id },
            Instant::now(),
        )
        .unwrap();
    r.drain();
    assert_eq!(duplicated.patterns.len(), renamed.patterns.len() + 1);
    assert_eq!(duplicated.selected, id);
    assert!(!r.c.lock().unwrap().live().playing);
}
#[test]
fn writer_and_engine_admission_refusals_preserve_reset_note_history_mode_and_tokens() {
    let mut r = rig();
    let b = r.channels[1];
    let v = r.s.sampler_trim_view(b).unwrap();
    let before = r.state();
    r.s.set_precommit_validator(Some(Arc::new(|_| Err("generated writer".into()))));
    assert!(r.s.sampler_trim_reset(b, &v.stamp, Instant::now()).is_err());
    assert_eq!(r.state(), before);
    assert!(
        r.s.apply(
            Edit::ToggleStep {
                channel: r.channels[0],
                step: 0
            },
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(r.state(), before);
    r.s.set_precommit_validator(None);
    r.reset(b);
    r.apply(Edit::ToggleStep {
        channel: r.channels[0],
        step: 0,
    });
    let capability = r.s.sampler_trim_view(b).unwrap();
    let stamp = r.s.arrangement_view();
    let before = r.state();
    while r.c.lock().unwrap().send(Command::Stop).is_ok() {}
    assert!(
        r.s.sampler_trim_undo(
            b,
            &capability.stamp,
            capability.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(r.state(), before);
    assert!(
        r.s.step_project_history(stamp.generation, stamp.revision, true, Instant::now())
            .is_err()
    );
    assert_eq!(r.state(), before);
    assert!(
        r.s.set_play_mode_in(
            stamp.generation,
            stamp.revision,
            PlayMode::Song,
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(r.state(), before);
    r.drain();
    assert!(matches!(r.step(true), HistoryEffect::Notes { .. }));
}

#[test]
fn absent_start_nondefault_end_roundtrips_real_global_inverse() {
    let mut r = rig();
    let b = r.channels[1];
    let mut project = r.s.project().clone();
    project.channels[1].sampler.remove("start");
    project.channels[1].sampler.insert("end".into(), 0.5);
    let old_generation = r.s.generation();
    r.s.open(
        Path::new("absent-inverse.grooviq"),
        Loaded {
            project,
            samples: r
                .channels
                .iter()
                .map(|id| {
                    (
                        *id,
                        SampleFile {
                            name: "generated.wav".into(),
                            bytes: Arc::new(wav()),
                        },
                    )
                })
                .collect(),
            warnings: vec![],
        },
        DecodedSamples {
            samples: r.channels.iter().map(|id| (*id, sample())).collect(),
            warnings: vec![],
            missing: BTreeSet::new(),
        },
    )
    .unwrap();
    r.drain();
    assert_eq!(r.s.generation(), old_generation + 1);
    assert_eq!(r.s.arrangement_view().revision, 0);
    assert_eq!(r.s.history_usage().0, 0);
    assert_eq!(
        r.s.sampler_trim_view(r.channels[0]).unwrap().frames,
        Some(2)
    );
    let original = r.s.project().channels[1].sampler.clone();
    assert!(!original.contains_key("start"));
    assert_eq!(original["end"].to_bits(), 0.5f32.to_bits());
    let initial = r.s.sampler_trim_view(b).unwrap();
    assert_eq!(initial.start.to_bits(), 0.0f32.to_bits());
    assert_eq!(initial.end.to_bits(), 0.5f32.to_bits());
    assert_eq!(initial.frames, Some(2));
    assert!(initial.undo.is_none());
    let reset =
        r.s.sampler_trim_reset(b, &initial.stamp, Instant::now())
            .unwrap();
    r.drain();
    assert!(reset.changed);
    assert!(reset.state.undo.is_some());
    assert_ne!(reset.state.stamp, initial.stamp);
    assert_eq!(r.s.arrangement_view().revision, 1);
    assert_eq!(r.s.history_usage().0, 1);
    let mut observations =
        vec![json!({ "phase":"reset", "trim": &reset.state, "state":r.state() })];
    let undo = step_trace(&mut r, true, &mut observations);
    assert!(matches!(undo, HistoryEffect::SamplerTrim { channel } if channel == b));
    assert_eq!(r.s.project().channels[1].sampler, original);
    assert!(!r.s.project().channels[1].sampler.contains_key("start"));
    assert_eq!(
        r.s.project().channels[1].sampler["end"].to_bits(),
        0.5f32.to_bits()
    );
    let undone = r.s.sampler_trim_view(b).unwrap();
    assert_ne!(undone.stamp, reset.state.stamp);
    assert!(undone.undo.is_none());
    assert_eq!(r.s.arrangement_view().revision, 2);
    assert_eq!(r.s.history_usage().0, 1);
    let redo = step_trace(&mut r, false, &mut observations);
    assert!(matches!(redo, HistoryEffect::SamplerTrim { channel } if channel == b));
    let redone = r.s.sampler_trim_view(b).unwrap();
    assert_eq!(
        r.s.project().channels[1]
            .sampler
            .get("start")
            .map(|v| v.to_bits()),
        Some(0.0f32.to_bits())
    );
    assert_eq!(
        r.s.project().channels[1]
            .sampler
            .get("end")
            .map(|v| v.to_bits()),
        Some(1.0f32.to_bits())
    );
    assert_eq!(redone.undo, reset.state.undo);
    assert_ne!(redone.stamp, undone.stamp);
    assert_eq!(r.s.arrangement_view().revision, 3);
    assert_eq!(r.s.history_usage().0, 1);
    assert_eq!(r.s.generation(), old_generation + 1);
    let before_refusal = r.state();
    assert!(
        r.s.sampler_trim_undo(
            b,
            &reset.state.stamp,
            reset.state.undo.as_ref().unwrap(),
            Instant::now()
        )
        .is_err()
    );
    assert_eq!(r.state(), before_refusal);
    eprintln!("m3-workspace-absence-trace:{}", json!(observations));
}
