use grooviq_engine::{self as e, media_budget::MediaBudget};
use grooviq_project::{
    arrangement_edit::ArrangementEdit,
    audio_assets::{AudioAssets, PreparedAudio},
    session::{MediaTarget, Session},
    *,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
fn wav(frames: u32) -> Vec<u8> {
    let data = frames * 2;
    let mut b = vec![];
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48000u32.to_le_bytes());
    b.extend_from_slice(&96000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for i in 0..frames {
        b.extend_from_slice(&((i % 300) as i16 * 50).to_le_bytes());
    }
    b.shrink_to_fit();
    b
}
fn rig(
    limit: usize,
) -> (
    Session,
    AudioAssets,
    e::AudioProcessor,
    Arc<Mutex<e::Controller>>,
) {
    let (c, mut p) = e::engine(e::EngineConfig {
        sample_rate: 48000.0,
    });
    let c = Arc::new(Mutex::new(c));
    let mut s = Session::new(c.clone());
    let budget = MediaBudget::with_limit(limit);
    s.set_media_budget(budget.clone());
    let assets = AudioAssets::with_budget(budget);
    let v = s.arrangement_view();
    s.edit_arrangement(
        v.generation,
        v.revision,
        vec![ArrangementEdit::CreateTrack {
            name: "T".into(),
            color: 0,
        }],
        Instant::now(),
    )
    .unwrap();
    p.process(&mut [0.0; 128], 2);
    (s, assets, p, c)
}
fn prepare(
    s: &mut Session,
    assets: &AudioAssets,
    target: MediaTarget,
    file: SampleFile,
) -> (grooviq_project::session::MediaIntent, Arc<PreparedAudio>) {
    let v = s.arrangement_view();
    let intent = s.media_intent(v.generation, v.revision, target).unwrap();
    let request = assets
        .submit_cancellable(
            0,
            intent.key(),
            intent.request(),
            file,
            intent.cancellation(),
        )
        .unwrap();
    s.accept_media_request(&intent).unwrap();
    let prepared = request.receive().unwrap();
    (intent, prepared)
}
fn import(track: u32) -> MediaTarget {
    MediaTarget::Import {
        track,
        start_tick: 0,
        name: "Audio".into(),
    }
}
fn file(frames: u32) -> SampleFile {
    SampleFile {
        name: "generated.wav".into(),
        bytes: Arc::new(wav(frames)),
    }
}
fn sound(s: &Session) -> Vec<f32> {
    e::render_offline_with(
        e::EngineConfig {
            sample_rate: 48000.0,
        },
        s.checked_export_state().unwrap(),
        &[(0, e::Command::Play)],
        5000,
        64,
    )
}
#[test]
fn original_aliases_distinct_allocations_and_snapshot_lifetime_share_one_budget() {
    let budget = MediaBudget::with_limit(100);
    let a = Arc::new(vec![0u8; 40]);
    budget.original(&a).unwrap();
    budget.original(&a.clone()).unwrap();
    assert_eq!(budget.used(), 40);
    let b = Arc::new(vec![0u8; 40]);
    budget.original(&b).unwrap();
    assert_eq!(budget.used(), 80);
    assert!(budget.reserve(21).is_err());
    let exact = budget.reserve(20).unwrap();
    assert_eq!(budget.used(), 100);
    drop(exact);
    drop(b);
    let snapshot = a.clone();
    drop(a);
    assert_eq!(budget.used(), 40);
    drop(snapshot);
    assert_eq!(budget.used(), 0);
}
#[test]
fn early_decode_and_peak_refusal_release_without_overshoot() {
    let bytes = wav(4096);
    let budget = MediaBudget::with_limit(8192);
    assert!(e::decode_sample_budgeted(&bytes, Some("wav"), 600.0, &budget).is_err());
    assert!(budget.high_water() <= 8192);
    assert_eq!(budget.used(), 0);
    let budget = MediaBudget::with_limit(65536);
    let sample = e::decode_sample_budgeted(&bytes, Some("wav"), 600.0, &budget).unwrap();
    let used = budget.used();
    assert!(used > 0);
    let blocker = budget.reserve(65536 - used).unwrap();
    assert!(e::waveform::PeakPyramid::budgeted(sample.clone(), &budget).is_err());
    assert_eq!(budget.used(), 65536);
    drop(blocker);
    let peaks = e::waveform::PeakPyramid::budgeted(sample, &budget).unwrap();
    assert!(budget.used() > used);
    drop(peaks);
    assert_eq!(budget.used(), 0);
}
#[test]
fn final_publication_fence_rejects_received_results_after_new_revision_cancel_and_supersession() {
    for scenario in 0..5 {
        let (mut s, assets, mut processor, _) = rig(1 << 20);
        let track = s.project().arrangement.tracks[0].id;
        let (intent, prepared) = prepare(&mut s, &assets, import(track), file(128));
        match scenario {
            0 => {
                s.new_project().unwrap();
            }
            1 => {
                let v = s.arrangement_view();
                s.edit_arrangement(
                    v.generation,
                    v.revision,
                    vec![ArrangementEdit::SetTrack {
                        track,
                        name: "Changed".into(),
                        color: 0,
                        mute: false,
                    }],
                    Instant::now(),
                )
                .unwrap();
            }
            2 => {
                s.cancel_media_request(intent.request());
            }
            3 => {
                let (_new, _prepared) = prepare(&mut s, &assets, import(track), file(129));
            }
            _ => {
                let v = s.arrangement_view();
                s.edit_arrangement(
                    v.generation,
                    v.revision,
                    vec![ArrangementEdit::DeleteTrack { track }],
                    Instant::now(),
                )
                .unwrap();
            }
        }
        let before = s.project().clone();
        let view = s.view();
        let usage = s.history_usage();
        assert!(s.publish_media(&intent, prepared, Instant::now()).is_err());
        assert_eq!(s.project(), &before);
        assert_eq!(s.view(), view);
        assert_eq!(s.history_usage(), usage);
        processor.process(&mut [0.0; 128], 2);
    }
}
#[test]
fn import_relink_history_snapshot_retirement_and_original_save_reopen_export() {
    let (mut s, assets, mut processor, c) = rig(1 << 20);
    let track = s.project().arrangement.tracks[0].id;
    let (intent, prepared) = prepare(&mut s, &assets, import(track), file(4800));
    let ack = s.publish_media(&intent, prepared, Instant::now()).unwrap();
    let asset = ack.created_ids[0];
    s.set_play_mode(PlayMode::Song, Instant::now()).unwrap();
    let before = sound(&s);
    assert!(before.iter().any(|x| *x != 0.0));
    processor.process(&mut [0.0; 128], 2);
    let snapshot = s.checked_export_state().unwrap();
    let budget = s.media_budget();
    let used = budget.used();
    let (intent, prepared) = prepare(&mut s, &assets, MediaTarget::Relink { asset }, file(4000));
    s.publish_media(&intent, prepared, Instant::now()).unwrap();
    let after = sound(&s);
    assert_ne!(before, after);
    assert!(s.media_warnings().iter().any(|w| w.contains("provenance")));
    assert!(budget.used() > used);
    let v = s.arrangement_view();
    s.step_project_history(v.generation, v.revision, true, Instant::now())
        .unwrap();
    assert_eq!(sound(&s), before);
    let v = s.arrangement_view();
    s.step_project_history(v.generation, v.revision, false, Instant::now())
        .unwrap();
    assert_eq!(sound(&s), after);
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("audio.grooviq");
    let job = s.begin_save(Some(&path)).unwrap();
    let result = job.write(&d.path().join("backups"));
    s.end_save(&job, result).unwrap();
    let loaded = load_budgeted(&path, &budget).unwrap();
    assert_eq!(*loaded.samples[&asset].bytes, wav(4000));
    let metadata = loaded.project.arrangement.audio_assets[0].clone();
    assert_eq!(
        metadata.source_frames, 4800,
        "relink preserves saved source provenance"
    );
    let decoded = decode_samples_budgeted(&loaded, &budget);
    let report = s.open(&path, loaded, decoded).unwrap();
    assert!(report.warnings.iter().any(|w| w.contains("geometry")));
    assert_eq!(sound(&s), after);
    let export = s.checked_export_state().unwrap();
    let options = e::ExportOptions {
        max_tail_seconds: 0.0,
        ..Default::default()
    };
    let exported = e::export_wav(
        export,
        &options,
        &d.path().join("audio.wav"),
        &mut |_| {},
        &std::sync::atomic::AtomicBool::new(false),
    )
    .unwrap();
    assert!(exported.frames > 0);
    s.new_project().unwrap();
    assert!(
        budget.used() > 0,
        "old save/export/engine retains allocations"
    );
    drop(job);
    drop(snapshot);
    processor.process(&mut [0.0; 128], 2);
    c.lock().unwrap().check_health();
    assert_eq!(budget.used(), 0);
}
#[test]
fn queue_refusal_media_does_not_burn_ids_or_history_and_valid_retry_works() {
    let (mut s, assets, mut processor, c) = rig(1 << 20);
    let track = s.project().arrangement.tracks[0].id;
    let (intent, prepared) = prepare(&mut s, &assets, import(track), file(128));
    while c
        .lock()
        .unwrap()
        .send(e::Command::SetTempo { bpm: 140.0 })
        .is_ok()
    {}
    let before = s.project().clone();
    let usage = s.history_usage();
    assert!(
        s.publish_media(&intent, prepared.clone(), Instant::now())
            .is_err()
    );
    assert_eq!(s.project(), &before);
    assert_eq!(s.history_usage(), usage);
    processor.process(&mut [0.0; 128], 2);
    s.publish_media(&intent, prepared, Instant::now()).unwrap();
    assert_eq!(s.project().arrangement.clips.len(), 1);
}
#[test]
fn original_read_and_embedded_load_reserve_before_growth() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("generated.wav");
    std::fs::write(&path, wav(128)).unwrap();
    let tiny = MediaBudget::with_limit(100);
    assert!(read_sample_file_budgeted(&path, &tiny).is_err());
    assert_eq!(tiny.used(), 0);
    assert!(tiny.high_water() <= 100);
    let shared = MediaBudget::with_limit(4096);
    let original = read_sample_file_budgeted(&path, &shared).unwrap();
    let mut p = Project::new();
    p.arrangement.audio_assets.push(AudioAsset {
        id: 1,
        sample: SampleRef {
            path: "original.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 128,
        original_tempo: None,
        original_key: None,
    });
    p.next_id = 2;
    let zip = d.path().join("p.grooviq");
    save(&p, &HashMap::from([(1, original)]), &zip).unwrap();
    assert!(load_budgeted(&zip, &tiny).is_err());
    assert_eq!(tiny.used(), 0);
}

#[test]
fn removed_asset_history_pins_media_until_eviction_and_retired_bank_release() {
    let (mut s, assets, mut processor, c) = rig(1 << 20);
    let track = s.project().arrangement.tracks[0].id;
    let (intent, prepared) = prepare(&mut s, &assets, import(track), file(1024));
    let ack = s.publish_media(&intent, prepared, Instant::now()).unwrap();
    let asset = ack.created_ids[0];
    let clip = ack.created_ids[1];
    processor.process(&mut [0.0; 128], 2);
    c.lock().unwrap().check_health();
    let budget = s.media_budget();
    let usage = budget.used();
    let v = s.arrangement_view();
    s.edit_arrangement(
        v.generation,
        v.revision,
        vec![
            ArrangementEdit::DeleteClip { clip },
            ArrangementEdit::DeleteAudioAsset { asset },
        ],
        Instant::now(),
    )
    .unwrap();
    assert_eq!(budget.used(), usage);
    let v = s.arrangement_view();
    s.step_project_history(v.generation, v.revision, true, Instant::now())
        .unwrap();
    assert!(s.waveform_source(s.generation(), asset).is_ok());
    let v = s.arrangement_view();
    s.step_project_history(v.generation, v.revision, false, Instant::now())
        .unwrap();
    s.set_history_limits(HistoryLimits {
        steps: 1,
        ..Default::default()
    });
    let v = s.arrangement_view();
    s.edit_arrangement(
        v.generation,
        v.revision,
        vec![ArrangementEdit::CreateTrack {
            name: "Evict".into(),
            color: 0,
        }],
        Instant::now(),
    )
    .unwrap();
    processor.process(&mut [0.0; 128], 2);
    c.lock().unwrap().check_health();
    assert_eq!(budget.used(), 0);
}
#[test]
fn cache_hit_alias_near_quota_uses_existing_decoded_and_peak_allocations() {
    let budget = MediaBudget::with_limit(1 << 20);
    let assets = AudioAssets::with_budget(budget.clone());
    let original = file(1024);
    let first = assets
        .submit(0, 1, 1, original.clone())
        .unwrap()
        .receive()
        .unwrap();
    let used = budget.used();
    let block = budget.reserve((1 << 20) - used).unwrap();
    let second = assets.submit(0, 2, 2, original).unwrap().receive().unwrap();
    assert!(Arc::ptr_eq(&first.peaks, &second.peaks));
    assert_eq!(budget.used(), 1 << 20);
    drop(block);
    drop(first);
    drop(second);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_flac_import_preserves_original_and_waveform_actual_geometry() {
    let (mut s, assets, _, _) = rig(1 << 20);
    let track = s.project().arrangement.tracks[0].id;
    let bytes = Arc::new(include_bytes!("../../engine/tests/data/blip.flac").to_vec());
    let (intent, prepared) = prepare(
        &mut s,
        &assets,
        import(track),
        SampleFile {
            name: "generated.flac".into(),
            bytes: bytes.clone(),
        },
    );
    let ack = s.publish_media(&intent, prepared, Instant::now()).unwrap();
    let asset = ack.created_ids[0];
    let (sample, peaks) = s.waveform_source(s.generation(), asset).unwrap();
    assert_eq!(sample.rate, 44100.0);
    assert_eq!(sample.len(), 4410);
    let packet = peaks
        .unwrap()
        .encode(s.generation(), 123, asset, 0, sample.len(), 16)
        .unwrap();
    assert_eq!(&packet[..4], b"WFP1");
    assert_eq!(u64::from_le_bytes(packet[24..32].try_into().unwrap()), 4410);
    assert_eq!(packet.len(), 64 + 16 * 16);
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("flac.grooviq");
    let job = s.begin_save(Some(&path)).unwrap();
    job.write(&d.path().join("backups")).unwrap();
    assert_eq!(load(&path).unwrap().samples[&asset].bytes, bytes);
}
#[test]
fn combined_live_channel_and_asset_quota_cannot_be_evaded_by_new_project() {
    let (mut s, assets, mut processor, c) = rig(1 << 20);
    let track = s.project().arrangement.tracks[0].id;
    let original = file(1024);
    let sample =
        e::decode_sample_budgeted(&original.bytes, Some("wav"), 600.0, &s.media_budget()).unwrap();
    let own = s
        .add_channel(
            std::path::Path::new("channel.wav"),
            original.clone(),
            sample.clone(),
            Instant::now(),
        )
        .unwrap();
    assert_eq!(own.channels.len(), 1);
    let charged = s.media_budget().used();
    assert!(charged >= original.bytes.capacity() + sample.l.capacity() * 8);
    let (intent, prepared) = prepare(&mut s, &assets, import(track), original);
    assert!(
        !Arc::ptr_eq(&sample, prepared.peaks.sample()),
        "independent decode is a distinct allocation"
    );
    s.publish_media(&intent, prepared, Instant::now()).unwrap();
    let budget = s.media_budget();
    assert!(budget.used() > charged);
    let snapshot = s.export_state();
    s.new_project().unwrap();
    assert!(budget.used() > 0);
    drop(snapshot);
    drop(sample);
    processor.process(&mut [0.0; 128], 2);
    c.lock().unwrap().check_health();
    assert_eq!(budget.used(), 0);
}

#[test]
fn failed_media_limit_import_is_atomic_and_later_small_import_succeeds() {
    let (mut s, assets, _, _) = rig(8192);
    let track = s.project().arrangement.tracks[0].id;
    let before = s.project().clone();
    let v = s.arrangement_view();
    let intent = s
        .media_intent(v.generation, v.revision, import(track))
        .unwrap();
    let request = assets.submit_cancellable(
        0,
        intent.key(),
        intent.request(),
        file(4096),
        intent.cancellation(),
    );
    assert!(request.is_err() || request.unwrap().receive().is_err());
    assert_eq!(s.project(), &before);
    let (intent, prepared) = prepare(&mut s, &assets, import(track), file(32));
    s.publish_media(&intent, prepared, Instant::now()).unwrap();
    assert_eq!(s.project().arrangement.clips.len(), 1);
}

#[test]
fn later_busy_submission_does_not_cancel_accepted_session_request() {
    let (mut s, assets, _, _) = rig(65536);
    let track = s.project().arrangement.tracks[0].id;
    let (intent, prepared) = prepare(&mut s, &assets, import(track), file(128));
    let budget = s.media_budget();
    let used = budget.used();
    let hold = budget.reserve(65536 - used).unwrap();
    let v = s.arrangement_view();
    let later = s
        .media_intent(v.generation, v.revision, import(track))
        .unwrap();
    let refused = assets.submit_cancellable(
        0,
        later.key(),
        later.request(),
        file(129),
        later.cancellation(),
    );
    assert!(refused.is_err());
    drop(hold);
    s.publish_media(&intent, prepared, Instant::now()).unwrap();
    assert_eq!(s.project().arrangement.clips.len(), 1);
}
#[test]
fn decoder_quota_failure_on_embedded_open_is_refusal_not_missing_media_repair() {
    let loaded = Loaded {
        project: Project::new(),
        samples: HashMap::new(),
        warnings: vec![],
    };
    assert!(decode_samples_budgeted_checked(&loaded, &MediaBudget::with_limit(0)).is_ok());
    let mut project = Project::new();
    project.arrangement.audio_assets.push(AudioAsset {
        id: 1,
        sample: SampleRef {
            path: "large.wav".into(),
            file: None,
        },
        source_rate: 48000,
        source_frames: 4096,
        original_tempo: None,
        original_key: None,
    });
    project.next_id = 2;
    let loaded = Loaded {
        project,
        samples: HashMap::from([(1, file(4096))]),
        warnings: vec![],
    };
    let budget = MediaBudget::with_limit(10000);
    assert!(decode_samples_budgeted_checked(&loaded, &budget).is_err());
    assert!(budget.high_water() <= 10000);
}

#[test]
fn older_file_read_cannot_supersede_a_newer_accepted_request_even_after_failure_cancel() {
    let (mut s, assets, _, _) = rig(1 << 20);
    let track = s.project().arrangement.tracks[0].id;
    let v = s.arrangement_view();
    let older = s
        .media_intent(v.generation, v.revision, import(track))
        .unwrap();
    let newer = s
        .media_intent(v.generation, v.revision, import(track))
        .unwrap();
    let request = assets
        .submit_cancellable(
            0,
            newer.key(),
            newer.request(),
            file(128),
            newer.cancellation(),
        )
        .unwrap();
    s.accept_media_request(&newer).unwrap();
    s.cancel_media_request(newer.request());
    assert!(s.check_media_intent(&older).is_err());
    drop(request);
}
