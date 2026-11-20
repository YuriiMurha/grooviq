use grooviq_dsp::sampler::SampleBuf;
use grooviq_engine::{AudioProcessor, Command, Controller, EngineConfig, engine};
use grooviq_project::{
    DecodedSamples, Loaded, Project, SampleFile,
    arrangement_edit::HistoryEffect,
    session::{Edit, Session},
};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Clone, Copy, Debug)]
enum Replacement {
    New,
    Open,
    Recovery,
}

const REPLACEMENTS: [Replacement; 3] = [Replacement::New, Replacement::Open, Replacement::Recovery];

fn file() -> SampleFile {
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
    bytes.extend(8192i16.to_le_bytes());
    bytes.extend(8192i16.to_le_bytes());
    SampleFile {
        name: "generated.wav".into(),
        bytes: Arc::new(bytes),
    }
}

fn sample() -> Arc<SampleBuf> {
    Arc::new(SampleBuf {
        rate: 48000.0,
        l: vec![0.25; 2],
        r: vec![0.25; 2],
    })
}

fn empty_history_usage() -> (usize, usize, usize) {
    let (controller, _processor) = engine(EngineConfig {
        sample_rate: 48000.0,
    });
    let empty = Session::new(Arc::new(Mutex::new(controller)));
    let usage = empty.history_usage();
    assert_eq!((usage.0, usage.1), (0, 0));
    usage
}

struct Rig {
    session: Session,
    controller: Arc<Mutex<Controller>>,
    processor: AudioProcessor,
    channel: u32,
}

#[derive(Debug, PartialEq)]
struct Snapshot {
    project: Project,
    view: Value,
    generation: u64,
    arrangement_revision: u64,
    usage: (usize, usize, usize),
    trim: Value,
}

impl Rig {
    fn new() -> Self {
        let (controller, processor) = engine(EngineConfig {
            sample_rate: 48000.0,
        });
        let controller = Arc::new(Mutex::new(controller));
        let session = Session::new(controller.clone());
        let mut rig = Self {
            session,
            controller,
            processor,
            channel: 0,
        };
        rig.add_sample();
        rig.crossed_trim();
        rig
    }

    fn drain(&mut self) {
        self.processor.process(&mut [0.0; 128], 2);
    }

    fn add_sample(&mut self) {
        let view = self
            .session
            .add_channel(Path::new("generated.wav"), file(), sample(), Instant::now())
            .unwrap();
        self.channel = view.channels.last().unwrap().id;
        self.drain();
    }

    fn crossed_trim(&mut self) {
        for (param, value) in [("start", 0.6), ("end", 0.4)] {
            self.session
                .apply(
                    Edit::SetInstrumentParam {
                        channel: self.channel,
                        param: param.into(),
                        value,
                    },
                    Instant::now(),
                )
                .unwrap();
            self.drain();
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            project: self.session.project().clone(),
            view: serde_json::to_value(self.session.view()).unwrap(),
            generation: self.session.generation(),
            arrangement_revision: self.session.arrangement_view().revision,
            usage: self.session.history_usage(),
            trim: serde_json::to_value(self.session.sampler_trim_view(self.channel).unwrap())
                .unwrap(),
        }
    }

    fn step(&mut self, undo: bool) -> HistoryEffect {
        let stamp = self.session.arrangement_view();
        let effect = self
            .session
            .step_project_history(stamp.generation, stamp.revision, undo, Instant::now())
            .unwrap()
            .effect;
        self.drain();
        effect
    }

    fn assert_trim_effect(&mut self, undo: bool) {
        let channel = self.channel;
        assert!(
            matches!(self.step(undo), HistoryEffect::SamplerTrim { channel: id } if id == channel)
        );
    }

    fn seed_history(&mut self, redo: bool) -> (String, String) {
        let current = self.session.sampler_trim_view(self.channel).unwrap();
        let repaired = self
            .session
            .sampler_trim_reset(self.channel, &current.stamp, Instant::now())
            .unwrap();
        assert!(repaired.changed, "the fixture must create a real inverse");
        self.drain();
        self.assert_trim_effect(true);
        assert_eq!(
            self.session.project().channels[0].sampler["start"].to_bits(),
            0.6f32.to_bits()
        );
        self.assert_trim_effect(false);
        let active = self.session.sampler_trim_view(self.channel).unwrap();
        let token = active.undo.clone().expect("a real undo capability exists");
        if redo {
            self.assert_trim_effect(true);
            assert!(
                self.session
                    .sampler_trim_view(self.channel)
                    .unwrap()
                    .undo
                    .is_none()
            );
        }
        let usage = self.session.history_usage();
        assert_eq!(usage.0, 1, "the source history is nonempty for {redo:?}");
        assert!(
            usage.2 > empty_history_usage().2,
            "the retained entry is charged above empty bookkeeping"
        );
        (active.stamp, token)
    }

    fn incoming(&self, full_region: bool) -> (Loaded, DecodedSamples) {
        let mut project = self.session.project().clone();
        if full_region {
            project.channels[0].sampler.remove("start");
            project.channels[0].sampler.insert("end".into(), 1.0);
        }
        (
            Loaded {
                project,
                samples: HashMap::from([(self.channel, file())]),
                warnings: vec![],
            },
            DecodedSamples {
                samples: HashMap::from([(self.channel, sample())]),
                warnings: vec![],
                missing: BTreeSet::new(),
            },
        )
    }

    fn replace(&mut self, kind: Replacement, full_region: bool) -> Result<(), String> {
        let result = match kind {
            Replacement::New => self.session.new_project().map(|_| ()),
            Replacement::Open => {
                let (loaded, decoded) = self.incoming(full_region);
                self.session
                    .open(Path::new("replacement.grooviq"), loaded, decoded)
                    .map(|_| ())
            }
            Replacement::Recovery => {
                let (loaded, decoded) = self.incoming(full_region);
                self.session
                    .open_recovered(
                        Some(Path::new("replacement.grooviq")),
                        loaded,
                        decoded,
                        Instant::now(),
                    )
                    .map(|_| ())
            }
        };
        result.map_err(|error| error.to_string())
    }

    fn assert_replacement_retired_history(
        &mut self,
        old_generation: u64,
        old_capability: &(String, String),
    ) {
        assert_eq!(self.session.generation(), old_generation + 1);
        assert_eq!(self.session.history_usage(), empty_history_usage());
        assert!(matches!(self.step(true), HistoryEffect::Empty));
        assert!(matches!(self.step(false), HistoryEffect::Empty));
        assert_eq!(self.session.history_usage(), empty_history_usage());
        if self.session.project().channels.is_empty() {
            self.add_sample();
        } else {
            let current = self.session.sampler_trim_view(self.channel).unwrap();
            let no_op = self
                .session
                .sampler_trim_reset(self.channel, &current.stamp, Instant::now())
                .unwrap();
            assert!(
                !no_op.changed,
                "this reset must not hide the replacement bug"
            );
            assert!(matches!(self.step(true), HistoryEffect::Empty));
        }
        assert!(
            self.session
                .sampler_trim_undo(
                    self.channel,
                    &old_capability.0,
                    &old_capability.1,
                    Instant::now(),
                )
                .is_err()
        );
        self.crossed_trim();
        let current = self.session.sampler_trim_view(self.channel).unwrap();
        let new = self
            .session
            .sampler_trim_reset(self.channel, &current.stamp, Instant::now())
            .unwrap();
        assert!(new.changed);
        assert_ne!(new.state.stamp, old_capability.0);
        assert_ne!(new.state.undo.as_ref().unwrap(), &old_capability.1);
        self.drain();
        self.session
            .sampler_trim_undo(
                self.channel,
                &new.state.stamp,
                new.state.undo.as_ref().unwrap(),
                Instant::now(),
            )
            .unwrap();
        self.drain();
        let params = &self.session.project().channels[0].sampler;
        assert_eq!(params["start"].to_bits(), 0.6f32.to_bits());
        assert_eq!(params["end"].to_bits(), 0.4f32.to_bits());
    }

    fn replay_after_refusal(&mut self, redo: bool) {
        self.assert_trim_effect(!redo);
        self.assert_trim_effect(redo);
        assert_eq!(self.session.history_usage().0, 1);
    }
}

fn accepted_replacement(kind: Replacement) {
    for redo in [false, true] {
        let mut rig = Rig::new();
        let capability = rig.seed_history(redo);
        let generation = rig.session.generation();
        rig.replace(kind, true).unwrap();
        rig.drain();
        rig.assert_replacement_retired_history(generation, &capability);
    }
}

#[test]
fn accepted_new_retires_nonempty_trim_undo_and_redo() {
    accepted_replacement(Replacement::New);
}

#[test]
fn accepted_open_retires_nonempty_trim_undo_and_redo() {
    accepted_replacement(Replacement::Open);
}

#[test]
fn accepted_recovery_retires_nonempty_trim_undo_and_redo() {
    accepted_replacement(Replacement::Recovery);
}

#[test]
fn second_open_then_full_region_noop_reset_has_empty_global_history() {
    let mut rig = Rig::new();
    rig.replace(Replacement::Open, false).unwrap();
    rig.drain();
    assert_eq!(rig.session.generation(), 1);
    let capability = rig.seed_history(false);
    rig.replace(Replacement::Open, true).unwrap();
    rig.drain();
    assert_eq!(rig.session.generation(), 2);
    assert_eq!(rig.session.arrangement_view().revision, 0);
    let current = rig.session.sampler_trim_view(rig.channel).unwrap();
    let no_op = rig
        .session
        .sampler_trim_reset(rig.channel, &current.stamp, Instant::now())
        .unwrap();
    assert!(!no_op.changed);
    // A no-op must not remove the evidence of an old-generation inverse.
    assert!(matches!(rig.step(true), HistoryEffect::Empty));
    assert_eq!(rig.session.history_usage(), empty_history_usage());
    assert!(
        rig.session
            .sampler_trim_undo(rig.channel, &capability.0, &capability.1, Instant::now(),)
            .is_err()
    );
}

#[test]
fn refused_engine_replacements_preserve_valid_inverse_and_allow_retry() {
    for kind in REPLACEMENTS {
        for redo in [false, true] {
            let mut rig = Rig::new();
            let capability = rig.seed_history(redo);
            let before = rig.snapshot();
            while rig.controller.lock().unwrap().send(Command::Stop).is_ok() {}
            assert!(
                rig.replace(kind, true).is_err(),
                "{kind:?} must enter and fail engine admission"
            );
            assert_eq!(rig.snapshot(), before, "{kind:?}, redo={redo}");
            rig.drain();
            rig.replay_after_refusal(redo);
            let generation = rig.session.generation();
            rig.replace(kind, true).unwrap();
            rig.drain();
            rig.assert_replacement_retired_history(generation, &capability);
        }
    }
}

#[test]
fn refused_media_replacements_preserve_valid_inverse_and_allow_retry() {
    for kind in [Replacement::Open, Replacement::Recovery] {
        for redo in [false, true] {
            let mut rig = Rig::new();
            let capability = rig.seed_history(redo);
            let before = rig.snapshot();
            let budget = rig.session.media_budget();
            let reservation = budget
                .reserve(grooviq_engine::media_budget::MEDIA_BYTES - budget.used())
                .unwrap();
            let refused = rig.replace(kind, true).unwrap_err();
            assert!(refused.contains("Retained media"), "{refused}");
            assert_eq!(rig.snapshot(), before, "{kind:?}, redo={redo}");
            drop(reservation);
            rig.replay_after_refusal(redo);
            let generation = rig.session.generation();
            rig.replace(kind, true).unwrap();
            rig.drain();
            rig.assert_replacement_retired_history(generation, &capability);
        }
    }
}
