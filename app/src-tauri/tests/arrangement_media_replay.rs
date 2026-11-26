//! Drives the same command core as the Tauri adapters without opening a device.
use grooviq_app::arrangement_core::{
    arrangement_curve, arrangement_edit, arrangement_history, arrangement_view,
    arrangement_waveform, audio_cancel, audio_cancel_ticket, prepare_media,
};
use grooviq_engine::{EngineConfig, engine};
use grooviq_project::{
    audio_assets::AudioAssets,
    session::{MediaTarget, Session, decode_samples_budgeted_checked},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

fn steps() -> Vec<Value> {
    let step = |command: &str, args: Value| json!({"command": command, "args": args});
    let view = || step("arrangement_view", json!({"offset": 0, "limit": 32}));
    let edit = |generation, revision, edits_json: &str| {
        step(
            "arrangement_edit",
            json!({"generation": generation, "revision": revision, "editsJson": edits_json}),
        )
    };
    let history = |generation, revision, undo| {
        step(
            "arrangement_history",
            json!({"generation": generation, "revision": revision, "undo": undo}),
        )
    };
    let import = |revision, track, path: &str, ticket: u64| {
        step(
            "audio_import",
            json!({"generation": 0, "revision": revision, "track": track, "startTick": 240,
            "name": "  Take 01 / edit  ", "path": path, "ticket": ticket}),
        )
    };
    let relink = |generation, revision, path: &str, ticket: u64| {
        step(
            "audio_relink",
            json!({"generation": generation, "revision": revision, "asset": 2, "path": path, "ticket": ticket}),
        )
    };
    let waveform = |generation, start, end| {
        step(
            "arrangement_waveform",
            json!({"generation": generation, "asset": 2, "request": 777, "start": start, "end": end, "bins": 2}),
        )
    };
    let audio_resize = |asset, source_start, source_end, gain, fade_in, fade_out| {
        json!({"kind":"resize", "clip":3, "startTick":480, "content":{
            "type":"audio", "asset":asset, "sourceStart":source_start, "sourceEnd":source_end,
            "gain":gain, "reverse":true, "fadeIn":fade_in, "fadeOut":fade_out
        }})
    };
    let edit_audio = |revision, ops: Vec<Value>| edit(0, revision, &json!(ops).to_string());
    let mut steps = vec![
        edit(0, 0, r#"[{"kind":"createTrack","name":"Audio","color":0}]"#),
        import(0, 1, "/qa/generated.wav", 11),
        import(1, 2, "/qa/generated.wav", 12),
        step("audio_cancel_ticket", json!({"ticket": 0})),
        step(
            "audio_cancel_ticket",
            json!({"ticket": 9_007_199_254_740_992_u64}),
        ),
        import(1, 1, "/qa/never-read.wav", 0),
        import(1, 1, "/qa/never-read.wav", 9_007_199_254_740_992),
        step(
            "audio_cancel_ticket",
            json!({"ticket": 9_007_199_254_740_991_u64}),
        ),
        import(1, 1, "/qa/never-read.wav", 9_007_199_254_740_991),
        import(1, 1, "/qa/refused.wav", 13),
        view(),
        import(1, 1, "/qa/generated.wav", 13),
        step("audio_cancel", json!({"request": 5})),
        step("audio_cancel_ticket", json!({"ticket": 13})),
        view(),
        history(0, 2, true),
        view(),
        waveform(0, 1, 8),
        history(0, 3, false),
        waveform(0, 1, 8),
        edit(
            0,
            4,
            r#"[{"kind":"createTrack","name":"Patterns","color":1}]"#,
        ),
        edit(
            0,
            5,
            r#"[{"kind":"createClip","track":4,"name":"Pattern clip","startTick":0,"muted":false,"content":{"type":"pattern","pattern":0,"offsetTick":0,"lengthTicks":960}}]"#,
        ),
        relink(0, 6, "/qa/relinked.wav", 24),
        view(),
        waveform(0, 1, 8),
        waveform(0, 0, 3),
        history(0, 7, true),
        view(),
        waveform(0, 1, 8),
        history(0, 8, false),
        view(),
        waveform(0, 0, 3),
        // Relink has three decoded frames, but resizing and slicing use the saved eight.
        edit_audio(9, vec![audio_resize(2, 1, 9, 1.0, 0, 0)]),
        edit_audio(9, vec![audio_resize(2, 2, 2, 1.0, 0, 0)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, 1.0, 7, 0)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, 1.0, 0, 7)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, 2.1, 0, 0)]),
        edit_audio(9, vec![audio_resize(2, 1, 7, -0.1, 0, 0)]),
        edit_audio(9, vec![audio_resize(99, 1, 7, 1.0, 0, 0)]),
        edit(
            0,
            9,
            r#"[{"kind":"move","clip":3,"track":99,"startTick":480}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"move","clip":99,"track":98,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"copy","clip":99,"track":98,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"copy","clip":99,"track":4,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"move","clip":5,"track":98,"startTick":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"copy","clip":3,"track":4,"startTick":38400000}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":240,"audioFirstFrames":3}]"#,
        ),
        edit(0, 9, r#"[{"kind":"slice","clip":3,"tick":241}]"#),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":241,"audioFirstFrames":0}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":241,"audioFirstFrames":8}]"#,
        ),
        edit(
            0,
            9,
            r#"[{"kind":"slice","clip":3,"tick":242,"audioFirstFrames":3}]"#,
        ),
        edit_audio(
            9,
            vec![
                audio_resize(2, 7, 1, 1.0, 0, 0),
                json!({"kind":"slice","clip":3,"tick":481,"audioFirstFrames":3}),
            ],
        ),
        edit_audio(
            9,
            vec![
                json!({"kind":"copy","clip":3,"track":1,"startTick":720}),
                audio_resize(2, 1, 9, 1.0, 0, 0),
            ],
        ),
        view(),
        edit_audio(
            9,
            vec![
                json!({"kind":"move","clip":3,"track":4,"startTick":360}),
                audio_resize(2, 1, 7, 2.0, 5, 5),
                json!({"kind":"setClip","clip":3,"name":"Reverse take","muted":true}),
                json!({"kind":"copy","clip":3,"track":1,"startTick":720}),
            ],
        ),
        view(),
        edit(
            0,
            10,
            r#"[{"kind":"slice","clip":3,"tick":481,"audioFirstFrames":2},{"kind":"deleteClip","clip":6}]"#,
        ),
        view(),
        history(0, 11, true),
        view(),
        history(0, 12, false),
        view(),
        // Even with no visible clip delta, the batch issued an ID and owns one undo step.
        edit(
            0,
            13,
            r#"[{"kind":"copy","clip":3,"track":1,"startTick":720},{"kind":"deleteClip","clip":8}]"#,
        ),
        history(0, 14, true),
        view(),
        edit(
            0,
            15,
            r#"[{"kind":"copy","clip":5,"track":4,"startTick":960},{"kind":"deleteClip","clip":9}]"#,
        ),
        history(0, 16, true),
        view(),
        step("project_save_as", json!({"path": "/qa/media.grooviq"})),
        step("project_new", json!({})),
        step("project_open", json!({"path": "/qa/media.grooviq"})),
        view(),
        waveform(2, 0, 3),
        relink(2, 0, "/qa/mono.wav", 9_007_199_254_740_991),
        view(),
        waveform(2, 0, 4),
        history(2, 1, true),
        view(),
        waveform(2, 0, 3),
        history(2, 2, false),
        view(),
        waveform(2, 0, 4),
    ];
    steps.extend(automation_steps());
    steps.extend(automation_edit_steps());
    steps
}

fn automation_steps() -> Vec<Value> {
    let step = |command: &str, args: Value| json!({"command": command, "args": args});
    let view = || step("arrangement_view", json!({"offset": 0, "limit": 32}));
    let edit = |revision, ops: Vec<Value>| {
        step(
            "arrangement_edit",
            json!({"generation": 3, "revision": revision, "editsJson": json!(ops).to_string()}),
        )
    };
    let curve = |generation, revision, clip| {
        step(
            "arrangement_curve",
            json!({"generation": generation, "revision": revision, "clip": clip}),
        )
    };
    let history = |revision, undo| {
        step(
            "arrangement_history",
            json!({"generation": 3, "revision": revision, "undo": undo}),
        )
    };
    let content = json!({
        "type":"automation", "target":{"type":"channelVolume","channel":1}, "priority":99,
        "offsetTick":240, "lengthTicks":960,
        "points":[{"tick":0,"value":0.1,"mode":"hold"},{"tick":1200,"value":1.5,"mode":"linear"}]
    });
    let resize = |start, offset, length| {
        let mut content = content.clone();
        content["priority"] = json!(5);
        content["offsetTick"] = json!(offset);
        content["lengthTicks"] = json!(length);
        json!({"kind":"resize", "clip":4, "startTick":start, "content":content})
    };
    let mut retarget = resize(960, 240, 960);
    retarget["content"]["target"]["type"] = json!("channelPan");
    let mut reprioritize = resize(960, 240, 960);
    reprioritize["content"]["priority"] = json!(99);
    let mut reshape = resize(960, 240, 960);
    reshape["content"]["points"][0]["value"] = json!(0.5);
    let create = |content| json!({"kind":"createClip", "track":2, "name":"Rejected", "startTick":960, "muted":false,"content":content});
    let mut unknown_target = content.clone();
    unknown_target["target"]["channel"] = json!(99);
    let mut out_of_range = content.clone();
    out_of_range["points"][0]["value"] = json!(2.1);
    let mut unsorted = content.clone();
    unsorted["points"].as_array_mut().unwrap().reverse();
    let mut late_point = content.clone();
    late_point["points"][1]["tick"] = json!(38_400_001);
    let mut overflowing_offset = content.clone();
    overflowing_offset["offsetTick"] = json!(u32::MAX - 1);
    let mut synth_param = content.clone();
    synth_param["target"] =
        json!({"type":"instrumentParam","channel":1,"instrument":"synth","param":"osc1_level"});
    synth_param["points"][1]["value"] = json!(0.75);
    let mut wrong_instrument = synth_param.clone();
    wrong_instrument["target"]["instrument"] = json!("sampler");
    wrong_instrument["target"]["param"] = json!("start");
    let mut unknown_instrument = synth_param.clone();
    unknown_instrument["target"]["instrument"] = json!("drum");
    let mut unknown_param = synth_param.clone();
    unknown_param["target"]["param"] = json!("unknown");
    let param_resize = |instrument, param| {
        let mut content = synth_param.clone();
        content["target"]["instrument"] = json!(instrument);
        content["target"]["param"] = json!(param);
        content["priority"] = json!(13);
        json!({"kind":"resize", "clip":12, "startTick":960, "content":content})
    };
    vec![
        step("project_new", json!({})),
        step(
            "channel_add_synth",
            json!({"generation":3, "preset":"Init"}),
        ),
        edit(
            1,
            vec![
                json!({"kind":"createTrack","name":"Automation","color":0}),
                json!({"kind":"createTrack","name":"Layers","color":1}),
                json!({"kind":"createClipOnCreatedTrack","trackOperation":0,"name":"Volume","startTick":960,"muted":false,"content":content}),
                json!({"kind":"copy","clip":4,"track":3,"startTick":960}),
                json!({"kind":"createClip","track":2,"name":"Pattern","startTick":0,"muted":false,
                "content":{"type":"pattern","pattern":0,"offsetTick":0,"lengthTicks":960}}),
            ],
        ),
        view(),
        curve(3, 2, 4),
        curve(2, 2, 4),
        curve(3, 1, 4),
        curve(3, 2, 99),
        curve(3, 2, 8),
        edit(
            2,
            vec![json!({"kind":"resize","clip":8,"startTick":0,"content":content})],
        ),
        edit(
            2,
            vec![json!({"kind":"move","clip":99,"track":98,"startTick":0})],
        ),
        edit(
            2,
            vec![json!({"kind":"copy","clip":99,"track":3,"startTick":0})],
        ),
        edit(
            2,
            vec![json!({"kind":"move","clip":4,"track":98,"startTick":0})],
        ),
        edit(2, vec![resize(1080, 240, 840)]),
        edit(2, vec![retarget]),
        edit(2, vec![reprioritize]),
        edit(2, vec![reshape]),
        edit(2, vec![resize(0, 0, 960)]),
        edit(2, vec![resize(720, 0, 0)]),
        edit(
            2,
            vec![
                json!({"kind":"move","clip":4,"track":2,"startTick":0}),
                resize(0, 240, 38_399_761),
            ],
        ),
        edit(2, vec![resize(720, 0, 38_400_000)]),
        edit(2, vec![create(unknown_target)]),
        edit(2, vec![create(out_of_range)]),
        edit(2, vec![create(unsorted)]),
        edit(2, vec![create(late_point)]),
        edit(
            2,
            vec![
                create(overflowing_offset),
                json!({"kind":"slice","clip":9,"tick":963}),
            ],
        ),
        edit(2, vec![json!({"kind":"slice","clip":4,"tick":960})]),
        edit(2, vec![json!({"kind":"slice","clip":4,"tick":1920})]),
        edit(
            2,
            vec![json!({"kind":"slice","clip":4,"tick":1200,"audioFirstFrames":1})],
        ),
        edit(
            2,
            vec![
                json!({"kind":"move","clip":4,"track":2,"startTick":960}),
                resize(960, 240, 960),
                json!({"kind":"setClip","clip":4,"name":"Volume","muted":false}),
            ],
        ),
        edit(
            2,
            vec![
                json!({"kind":"move","clip":4,"track":3,"startTick":1440}),
                resize(1320, 120, 1080),
                json!({"kind":"copy","clip":4,"track":2,"startTick":1320}),
                json!({"kind":"slice","clip":4,"tick":1560,"audioFirstFrames":null}),
                json!({"kind":"setClip","clip":11,"name":"Tail","muted":true}),
                json!({"kind":"deleteClip","clip":6}),
            ],
        ),
        view(),
        curve(3, 3, 9),
        curve(3, 3, 11),
        curve(3, 3, 6),
        // Validation runs on the complete candidate. Moving the tail onto its same-priority
        // sibling on another track rolls back the rename too.
        edit(
            3,
            vec![
                json!({"kind":"setClip","clip":4,"name":"Must roll back","muted":true}),
                json!({"kind":"move","clip":11,"track":2,"startTick":1559}),
            ],
        ),
        view(),
        // Intermediate overlap is fine if the committed result is disjoint.
        edit(
            3,
            vec![
                json!({"kind":"move","clip":11,"track":3,"startTick":1559}),
                json!({"kind":"move","clip":11,"track":3,"startTick":1560}),
            ],
        ),
        history(3, true),
        view(),
        curve(3, 4, 6),
        history(4, false),
        view(),
        // Validation sorts by timeline position, independent of persisted clip order.
        edit(
            5,
            vec![json!({"kind":"move","clip":11,"track":3,"startTick":0})],
        ),
        edit(6, vec![create(wrong_instrument)]),
        edit(6, vec![create(unknown_instrument)]),
        edit(6, vec![create(unknown_param)]),
        edit(
            6,
            vec![
                json!({"kind":"createClip", "track":2, "name":"Oscillator 1", "startTick":960,
            "muted":false,"content":synth_param}),
            ],
        ),
        edit(7, vec![param_resize("synth", "osc2_level")]),
        edit(7, vec![param_resize("sampler", "osc1_level")]),
        step("project_save_as", json!({"path":"/qa/automation.grooviq"})),
        step("project_new", json!({})),
        step("project_open", json!({"path":"/qa/automation.grooviq"})),
        view(),
        curve(5, 0, 4),
        curve(5, 0, 9),
        curve(5, 0, 11),
        curve(3, 0, 11),
    ]
}

fn automation_edit_steps() -> Vec<Value> {
    let step = |command: &str, args: Value| json!({"command": command, "args": args});
    let view = || step("arrangement_view", json!({"offset":0,"limit":32}));
    let edit_at = |generation, revision, ops: Vec<Value>| {
        step(
            "arrangement_edit",
            json!({"generation":generation,"revision":revision,"editsJson":json!(ops).to_string()}),
        )
    };
    let edit = |revision, ops| edit_at(6, revision, ops);
    let curve = |generation, revision, clip| {
        step(
            "arrangement_curve",
            json!({"generation":generation,"revision":revision,"clip":clip}),
        )
    };
    let history = |revision, undo| {
        step(
            "arrangement_history",
            json!({"generation":6,"revision":revision,"undo":undo}),
        )
    };
    let point = |tick, value, mode| json!({"tick":tick,"value":value,"mode":mode});
    let points = |clip, points: Vec<Value>| json!({"kind":"setAutomationPoints","clip":clip,"points":points});
    let settings = |clip, target: Value, priority: u32| json!({"kind":"setAutomationClip","clip":clip,"target":target,"priority":priority});
    let volume = || json!({"type":"channelVolume","channel":1});
    let pan = || json!({"type":"channelPan","channel":1});
    let param = |instrument, param| json!({"type":"instrumentParam","channel":1,"instrument":instrument,"param":param});
    let copy = |clip, start| json!({"kind":"copy","clip":clip,"track":2,"startTick":start});
    let create_track = || json!({"kind":"createTrack","name":"Allocator probe","color":0});
    let initial = vec![
        point(99, 0.1, "linear"),
        point(100, 0.2, "hold"),
        point(150, 1.5, "linear"),
        point(199, 0.8, "hold"),
        point(200, 0.3, "linear"),
    ];
    let create = |name, start| {
        json!({"kind":"createClip","track":2,"name":name,"startTick":start,
        "muted":false,"content":{"type":"automation","target":volume(),"priority":99,
        "offsetTick":100,"lengthTicks":100,"points":initial}})
    };
    let window = || vec![point(100, 0.4, "linear"), point(149, 0.7, "hold")];
    // Keep large point probes compact in the checked-in tape. Both runners expand the same runs.
    let boundary = |generation, revision, probes: Vec<Value>| {
        step(
            "arrangement_edit",
            json!({"generation":generation,"revision":revision,"pointRuns":probes}),
        )
    };
    let run =
        |op, repeats, count, start| json!({"op":op,"repeats":repeats,"count":count,"start":start});
    let large_clip = || {
        json!({"kind":"createClip","track":2,"name":"Large curve","startTick":8000,
        "muted":false,"content":{"type":"automation","target":volume(),"priority":0,
        "offsetTick":100,"lengthTicks":16384,"points":[point(0, 0.5, "hold")]}})
    };
    vec![
        step("project_new", json!({})),
        step("channel_add_synth", json!({"generation":6,"preset":"Init"})),
        edit(
            1,
            vec![
                json!({"kind":"createTrack","name":"Automation edits","color":0}),
                create("Shared", 960),
                create("Independent", 1920),
                copy(3, 2880),
                json!({"kind":"slice","clip":3,"tick":1010}),
                json!({"kind":"createClip","track":2,"name":"Notes","startTick":0,"muted":false,
                "content":{"type":"pattern","pattern":0,"offsetTick":0,"lengthTicks":960}}),
                create_track(),
            ],
        ),
        edit(
            2,
            vec![
                points(3, window()),
                settings(7, volume(), 12),
                create_track(),
            ],
        ),
        view(),
        curve(6, 3, 3),
        curve(6, 3, 7),
        curve(6, 3, 9),
        curve(6, 3, 5),
        edit(3, vec![points(3, window()), settings(7, volume(), 12)]),
        edit(3, vec![points(3, vec![]), points(3, window())]),
        history(3, true),
        view(),
        curve(6, 4, 7),
        // A no-op after Undo must leave the pending Redo available.
        edit(
            4,
            vec![
                points(3, vec![point(100, 0.2, "hold")]),
                settings(7, volume(), 8),
            ],
        ),
        history(4, false),
        view(),
        curve(6, 5, 9),
        edit(5, vec![points(9, vec![])]),
        curve(6, 6, 3),
        edit(
            6,
            vec![points(
                9,
                vec![point(150, 0.6, "hold"), point(199, 2.00000001, "linear")],
            )],
        ),
        curve(6, 7, 7),
        edit(7, vec![points(9, vec![point(149, 0.1, "hold")])]),
        edit(
            7,
            vec![points(
                9,
                vec![point(200, 0.1, "hold"), point(150, 0.1, "hold")],
            )],
        ),
        edit(
            7,
            vec![points(
                9,
                vec![point(150, 0.1, "hold"), point(150, 0.2, "linear")],
            )],
        ),
        edit(
            7,
            vec![points(
                9,
                vec![point(151, 0.1, "hold"), point(150, 0.2, "linear")],
            )],
        ),
        edit(7, vec![points(9, vec![point(150, -0.1, "hold")])]),
        edit(7, vec![points(9, vec![point(150, 2.01, "hold")])]),
        edit(7, vec![points(10, vec![])]),
        edit(7, vec![points(99, vec![])]),
        edit(7, vec![settings(10, volume(), 8)]),
        edit(7, vec![settings(99, volume(), 8)]),
        edit(6, vec![points(3, window())]),
        edit(6, vec![points(3, vec![point(100, 0.2, "step")])]),
        edit(6, vec![points(3, vec![point(-1, 0.2, "hold")])]),
        edit(6, vec![points(-1, vec![point(100, 0.2, "step")])]),
        edit(
            6,
            vec![points(
                3,
                vec![json!({"tick":100,"value":null,"mode":"hold"})],
            )],
        ),
        edit(6, vec![json!({"kind":"setAutomationPoints","clip":3})]),
        edit(
            6,
            vec![json!({"kind":"setAutomationClip","clip":5,"target":volume(),"priority":-1})],
        ),
        edit(
            6,
            vec![settings(
                5,
                json!({"type":"instrumentParam","channel":1,"instrument":"synth"}),
                6,
            )],
        ),
        edit(
            7,
            vec![settings(5, json!({"type":"trackVolume","channel":1}), 6)],
        ),
        edit(7, vec![settings(3, pan(), u32::MAX)]),
        edit(
            7,
            vec![json!({"kind":"deleteClip","clip":7}), settings(3, pan(), 4)],
        ),
        edit(7, vec![settings(5, volume(), 15)]),
        edit(7, vec![settings(5, volume(), 2)]),
        edit(7, vec![settings(5, pan(), 12)]),
        edit(
            7,
            vec![settings(5, json!({"type":"channelVolume","channel":99}), 6)],
        ),
        edit(7, vec![settings(5, param("sampler", "start"), 6)]),
        edit(7, vec![settings(5, param("synth", "missing"), 6)]),
        edit(7, vec![settings(5, pan(), 6)]),
        edit(
            7,
            vec![
                points(9, vec![point(150, 3.0, "hold")]),
                settings(5, volume(), 15),
            ],
        ),
        edit(
            7,
            vec![
                settings(5, volume(), 14),
                create_track(),
                points(9, vec![point(150, 0.25, "hold")]),
                points(5, vec![point(200, 0.1, "hold")]),
            ],
        ),
        edit(
            7,
            vec![
                points(9, vec![point(150, 0.25, "hold")]),
                points(5, vec![point(100, 3.0, "hold")]),
                create_track(),
            ],
        ),
        view(),
        curve(6, 7, 9),
        edit(7, vec![settings(5, volume(), 15)]),
        edit(
            7,
            vec![
                points(5, vec![point(100, -1.0, "hold"), point(199, 1.0, "linear")]),
                settings(5, pan(), 14),
                create_track(),
            ],
        ),
        edit(8, vec![points(5, vec![point(100, -1.01, "hold")])]),
        edit(8, vec![points(5, vec![point(100, 1.01, "hold")])]),
        edit(
            8,
            vec![
                points(5, vec![point(100, 0.0, "hold"), point(199, 1.0, "linear")]),
                settings(5, param("synth", "osc1_level"), 14),
            ],
        ),
        edit(9, vec![points(5, vec![point(100, 1.01, "hold")])]),
        // An inverse plus a copy must keep sharing through undo and redo.
        edit(
            9,
            vec![points(3, vec![]), points(3, window()), copy(3, 3840)],
        ),
        history(10, true),
        history(11, false),
        edit(12, vec![points(16, vec![point(100, 0.45, "linear")])]),
        curve(6, 13, 3),
        curve(6, 13, 9),
        step(
            "project_save_as",
            json!({"path":"/qa/edited-automation.grooviq"}),
        ),
        step("project_new", json!({})),
        step(
            "project_open",
            json!({"path":"/qa/edited-automation.grooviq"}),
        ),
        view(),
        curve(8, 0, 5),
        curve(8, 0, 7),
        curve(8, 0, 9),
        edit_at(
            8,
            0,
            vec![points(3, vec![]), settings(3, volume(), 18), create_track()],
        ),
        curve(8, 1, 3),
        curve(8, 1, 7),
        curve(8, 1, 9),
        boundary(8, 0, vec![run(points(99, vec![]), 1, 16_385, 100)]),
        boundary(
            8,
            0,
            vec![
                run(points(99, vec![]), 61, 16_384, 100),
                run(large_clip(), 1, 577, 100),
            ],
        ),
        boundary(
            8,
            0,
            vec![
                run(points(99, vec![]), 61, 16_384, 100),
                run(json!({"kind":"deleteClip","clip":99}), 1, 576, 100),
            ],
        ),
        boundary(
            8,
            0,
            vec![
                run(points(99, vec![]), 61, 16_384, 100),
                run(json!({"kind":"deleteClip","clip":99}), 1, 577, 100),
            ],
        ),
        boundary(
            8,
            1,
            vec![
                run(large_clip(), 1, 0, 0),
                run(points(20, vec![]), 1, 16_384, 100),
            ],
        ),
        boundary(
            8,
            1,
            vec![
                run(large_clip(), 1, 0, 0),
                run(points(20, vec![]), 1, 16_383, 100),
                run(copy(20, 24000), 61, 0, 0),
            ],
        ),
        edit_at(8, 1, vec![create_track()]),
        // No later operation in these batches can mask a missed priority allocation.
        edit_at(8, 2, vec![settings(5, param("synth", "osc1_level"), 21)]),
        edit_at(8, 3, vec![settings(5, param("synth", "osc1_level"), 22)]),
        {
            let mut overflowing = large_clip();
            overflowing["content"]["lengthTicks"] = json!(u64::MAX);
            edit_at(8, 4, vec![overflowing, points(23, vec![])])
        },
        {
            let mut overflowing = large_clip();
            overflowing["content"]["lengthTicks"] = json!(u64::MAX);
            edit_at(8, 4, vec![overflowing])
        },
        // EditFields validates supplied fields even when the selected operation ignores them.
        {
            let mut invalid_color = points(3, vec![point(100, 0.4, "linear")]);
            invalid_color["color"] = json!(256);
            edit_at(8, 4, vec![invalid_color])
        },
        // Keep the invalid mode first in the original JSON. Decoding precedes the stale stamp.
        step(
            "arrangement_edit",
            json!({"generation":8,"revision":0,"editsJson":
                r#"[{"kind":"setAutomationPoints","clip":3,"points":[{"mode":"step","tick":-1,"value":0.2}]}]"#}),
        ),
        // Optional loop data still uses its native decoder even when the operation ignores it.
        {
            let mut unused_region = points(3, vec![]);
            unused_region["region"] = json!([0, 960, true]);
            edit_at(8, 4, vec![unused_region])
        },
    ]
}

fn edits_json(args: &Value) -> String {
    if let Some(runs) = args["pointRuns"].as_array() {
        let mut ops = Vec::new();
        for run in runs {
            let mut op = run["op"].clone();
            let count = run["count"].as_u64().unwrap();
            if count != 0 {
                let start = run["start"].as_u64().unwrap();
                let points = json!(
                    (start..start + count)
                        .map(|tick| json!({"tick":tick,"mode":"hold","value":0.5}))
                        .collect::<Vec<_>>()
                );
                if op["content"]["type"] == "automation" {
                    op["content"]["points"] = points;
                } else {
                    op["points"] = points;
                }
            }
            ops.extend(std::iter::repeat_n(
                op,
                run["repeats"].as_u64().unwrap() as usize,
            ));
        }
        json!(ops).to_string()
    } else {
        args["editsJson"].as_str().unwrap().into()
    }
}

#[test]
fn arrangement_media_matches_shared_replay() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/arrangement-media-replay.json"
    );
    let mut fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let steps = steps();
    let update = std::env::var("GROOVIQ_UPDATE_REPLAY").as_deref() == Ok("1");
    if update {
        fixture["steps"] = json!(steps);
    }
    assert_eq!(json!(steps), fixture["steps"]);
    let dir = tempfile::tempdir().unwrap();
    let local = |path: &str| dir.path().join(Path::new(path).file_name().unwrap());
    for (virtual_path, bytes) in fixture["files"].as_object().unwrap() {
        let bytes = if bytes.is_null() {
            // A long enough file for probing, with no recognized container.
            vec![0; 128]
        } else {
            serde_json::from_value::<Vec<u8>>(bytes.clone()).unwrap()
        };
        std::fs::write(local(virtual_path), bytes).unwrap();
    }
    let (controller, mut processor) = engine(EngineConfig {
        sample_rate: 48_000.0,
    });
    let session = Mutex::new(Session::new(Arc::new(Mutex::new(controller))));
    let assets = AudioAssets::with_budget(session.lock().unwrap().media_budget());
    let mut actual = Vec::new();
    for step in &steps {
        let args = &step["args"];
        let n = |key: &str| args[key].as_u64().unwrap();
        let string = |key: &str| args[key].as_str().unwrap();
        let outcome: Result<Value, String> = match step["command"].as_str().unwrap() {
            "arrangement_edit" => Ok(json!(arrangement_edit(
                &session,
                n("generation"),
                n("revision"),
                &edits_json(args),
            ))),
            "arrangement_view" => {
                arrangement_view(&session, n("offset") as usize, n("limit") as usize)
            }
            "arrangement_curve" => {
                arrangement_curve(&session, n("generation"), n("revision"), n("clip") as u32)
            }
            "audio_import" | "audio_relink" => {
                let target = if step["command"] == "audio_import" {
                    MediaTarget::Import {
                        track: n("track") as u32,
                        start_tick: n("startTick"),
                        name: string("name").into(),
                    }
                } else {
                    MediaTarget::Relink {
                        asset: n("asset") as u32,
                    }
                };
                Ok(json!(tauri::async_runtime::block_on(prepare_media(
                    &session,
                    &assets,
                    n("generation"),
                    n("revision"),
                    target,
                    local(string("path")).to_str().unwrap().into(),
                    n("ticket"),
                ))))
            }
            "audio_cancel" => Ok(json!(audio_cancel(&session, n("request")))),
            "audio_cancel_ticket" => Ok(json!(audio_cancel_ticket(&session, n("ticket")))),
            "arrangement_history" => Ok(arrangement_history(
                &session,
                n("generation"),
                n("revision"),
                args["undo"].as_bool().unwrap(),
            )),
            "arrangement_waveform" => tauri::async_runtime::block_on(arrangement_waveform(
                &session,
                n("generation"),
                n("asset") as u32,
                n("request"),
                n("start") as usize,
                n("end") as usize,
                n("bins") as usize,
            ))
            .map(|bytes| json!(bytes)),
            // The arrangement reads capture the channel that the curve targets.
            "channel_add_synth" => session
                .lock()
                .unwrap()
                .add_synth_channel_in(n("generation"), string("preset"), Instant::now())
                .map(|_| Value::Null)
                .map_err(|error| error.to_string()),
            // Project command replies include host-dependent paths. Their effects are
            // captured by the following arrangement reads.
            "project_save_as" => {
                session
                    .lock()
                    .unwrap()
                    .save(Some(&local(string("path"))), dir.path())
                    .unwrap();
                Ok(Value::Null)
            }
            "project_new" => {
                session.lock().unwrap().new_project().unwrap();
                Ok(Value::Null)
            }
            "project_open" => {
                let path = local(string("path"));
                let mut session = session.lock().unwrap();
                let budget = session.media_budget();
                let loaded = grooviq_project::load_budgeted(&path, &budget).unwrap();
                let decoded = decode_samples_budgeted_checked(&loaded, &budget).unwrap();
                let opened = session.open(&path, loaded, decoded).unwrap();
                Ok(json!(opened.warnings))
            }
            other => panic!("unhandled arrangement media replay command {other}"),
        };
        actual.push(match outcome {
            Ok(value) => json!({"ok": value}),
            Err(error) => json!({"error": error}),
        });
        processor.process(&mut [0.0; 128], 2);
    }
    if update {
        fixture["expected"] = json!(actual);
        std::fs::write(path, serde_json::to_string_pretty(&fixture).unwrap() + "\n").unwrap();
    }
    assert_eq!(
        json!(actual),
        fixture["expected"],
        "real Session media replay drift"
    );
}
