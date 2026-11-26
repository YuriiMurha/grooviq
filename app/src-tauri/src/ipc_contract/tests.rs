use super::*;

const REGISTRY: &str = "
mod sample;
fn launch() {
    app.invoke_handler(tauri::generate_handler![sample::read]);
}";
const COMMAND: &str = r#"
use tauri::{AppHandle, State};
#[tauri::command]
async fn read(app: AppHandle, state: State<'_, Audio>, frame_count: Option<u32>) -> Result<(), String> {}
"#;

fn fixture(registry: &str, command: &str) -> Result<Vec<Command<'static>>, String> {
    commands(
        registry,
        &["sample::read"],
        &[],
        &mut Declarations::default(),
        |_| Ok(command.to_owned()),
    )
}

#[test]
fn a_command_reply_collects_its_dto_and_dependencies() {
    let mut declarations = Declarations::default();
    let source = COMMAND.replace(
        "Result<(), String>",
        "Result<Option<grooviq_project::session::OpenReport>, String>",
    );
    let commands = commands(REGISTRY, &["sample::read"], &[], &mut declarations, |_| {
        Ok(source.clone())
    })
    .unwrap();
    assert_eq!(
        commands[0].signature.as_ref().unwrap().reply,
        "OpenReport | null"
    );
    let mut output = String::new();
    declarations.write(&mut output).unwrap();
    for name in ["OpenReport", "ProjectView", "ChannelView", "TimeSignature"] {
        assert!(
            output.contains(&format!("export type {name} =")),
            "{output}"
        );
    }
    assert!(!output.contains("export type TrimView ="));
}

#[test]
fn uses_tauri_argument_casing_and_omits_optional_arguments() {
    let commands = fixture(REGISTRY, COMMAND).unwrap();
    let command = &commands[0];
    assert_eq!(command.name, "read");
    let signature = command.signature.as_ref().unwrap();
    assert_eq!(signature.args, ["frameCount?: number | null"]);
    assert_eq!(signature.reply, "null");

    let source = COMMAND.replace(
        "tauri::command",
        "tauri::command(rename_all = \"snake_case\")",
    );
    let commands = fixture(REGISTRY, &source).unwrap();
    let signature = commands[0].signature.as_ref().unwrap();
    assert_eq!(signature.args, ["frame_count?: number | null"]);
}

#[test]
fn sampler_metadata_serializes_missing_values_as_null() {
    let view = grooviq_project::session::sampler_trim::TrimView {
        stamp: "stamp".into(),
        start: 0.0,
        end: 1.0,
        frames: None,
        undo: None,
    };
    assert_eq!(
        serde_json::to_value(view).unwrap(),
        serde_json::json!({
            "stamp": "stamp", "start": 0.0, "end": 1.0, "frames": null, "undo": null
        })
    );
}

#[test]
fn rejects_unknown_types_and_unparseable_signatures() {
    for (ty, message) in [
        ("Missing", "unknown IPC type"),
        ("Option<", "sample.rs"),
        ("impostor::Option<u32>", "unknown IPC type"),
        ("u64", "unknown IPC type"),
    ] {
        let source = COMMAND.replace("Option<u32>", ty);
        let error = fixture(REGISTRY, &source).err().unwrap();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn inventory_rejects_unclassified_and_empty_registries() {
    let extra = REGISTRY.replace("sample::read", "sample::read, sample::unclassified");
    let empty = REGISTRY.replace("sample::read", "");
    let missing = REGISTRY.replace("sample::read", "sample::other");
    for (source, message) in [
        (extra, "unclassified command sample::unclassified"),
        (empty, "inventory is empty"),
        (missing, "unclassified command sample::other"),
    ] {
        let error = fixture(&source, COMMAND).err().unwrap();
        assert!(error.contains(message), "{error}");
    }
}

#[test]
fn inventory_rejects_classifications_absent_from_the_registry() {
    let error = commands(
        REGISTRY,
        &["sample::read", "sample::gone"],
        &[],
        &mut Declarations::default(),
        |_| Ok(COMMAND.to_owned()),
    )
    .err()
    .expect("sample::gone is no longer registered");
    assert!(
        error.contains("classified commands absent from generate_handler!")
            && error.contains("sample::gone"),
        "{error}"
    );
}

#[test]
fn comparison_accepts_crlf_and_rejects_shape_and_file_inventory_changes() {
    let expected = tempfile::tempdir().unwrap();
    let actual = tempfile::tempdir().unwrap();
    let file = "contract.ts";
    let source = "export type Reply = { frames: number };\n";
    fs::write(expected.path().join(file), source).unwrap();
    fs::write(actual.path().join(file), source.replace('\n', "\r\n")).unwrap();
    compare(expected.path(), actual.path()).unwrap();

    fs::write(actual.path().join(file), source.replace("number", "string")).unwrap();
    assert!(
        compare(expected.path(), actual.path())
            .unwrap_err()
            .contains("differs")
    );

    fs::write(actual.path().join(file), source).unwrap();
    fs::write(actual.path().join("obsolete.ts"), source).unwrap();
    assert!(
        compare(expected.path(), actual.path())
            .unwrap_err()
            .contains("inventory")
    );
    fs::remove_file(actual.path().join(file)).unwrap();
    fs::remove_file(actual.path().join("obsolete.ts")).unwrap();
    assert!(
        compare(expected.path(), actual.path())
            .unwrap_err()
            .contains("inventory")
    );
    fs::remove_file(expected.path().join(file)).unwrap();
    assert!(
        compare(expected.path(), actual.path())
            .unwrap_err()
            .contains("empty")
    );
}

#[test]
fn update_replaces_stale_contents_and_removes_obsolete_files() {
    let expected = tempfile::tempdir().unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let source = "export type Reply = { frames: number };\n";
    write_contract(expected.path(), source).unwrap();
    fs::write(checkout.path().join(FILE), "stale").unwrap();
    fs::write(checkout.path().join("obsolete.ts"), source).unwrap();
    assert!(
        compare(expected.path(), checkout.path())
            .unwrap_err()
            .contains("inventory differs")
    );

    update_contract(checkout.path(), source).unwrap();

    compare(expected.path(), checkout.path()).unwrap();
}
