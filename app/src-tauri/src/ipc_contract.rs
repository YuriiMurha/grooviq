//! Generate the JSON IPC types from Tauri registrations and the Rust wire DTOs.

mod signature;
use signature::{Command, commands};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};
use ts_rs::{Config, TS, TypeVisitor};

const MIGRATED: &[&str] = &[
    "notes_cmds::channel_add_synth",
    "project_cmds::channel_add",
    "project_cmds::channel_set_sample",
    "project_cmds::project_edit",
    "project_cmds::project_new",
    "project_cmds::project_open",
    "project_cmds::project_save",
    "project_cmds::project_save_as",
    "project_cmds::project_view",
    "project_cmds::recovery_restore",
    "project_cmds::transport_play",
    "project_cmds::transport_stop",
    "sampler_trim_cmds::sampler_trim_view",
];

// Each entry is an intentional migration exception. Keep the registered Rust path,
// so a move, removal or new command requires a decision here.
const DEFERRED: &[(&str, &str)] = &[
    ("arrangement_cmds::arrangement_view", "arrangement"),
    ("arrangement_cmds::arrangement_edit", "arrangement"),
    ("arrangement_cmds::arrangement_history", "arrangement"),
    ("arrangement_cmds::arrangement_curve", "arrangement"),
    ("arrangement_cmds::audio_import", "arrangement"),
    ("arrangement_cmds::audio_relink", "arrangement"),
    ("arrangement_cmds::audio_cancel", "arrangement"),
    ("arrangement_cmds::audio_cancel_ticket", "arrangement"),
    ("arrangement_cmds::arrangement_waveform", "binary"),
    ("arrangement_cmds::arrangement_mode", "arrangement"),
    ("arrangement_cmds::arrangement_loop", "arrangement"),
    ("arrangement_cmds::arrangement_seek", "arrangement"),
    ("mixer_cmds::mixer_view", "mixer"),
    ("mixer_cmds::mixer_edit", "mixer"),
    ("mixer_cmds::mixer_insert_detail", "mixer"),
    ("mixer_meter_cmd::mixer_meter", "mixer"),
    ("menu::edit_history_enabled", "shell"),
    ("commands::audio_outputs", "audio"),
    ("commands::audio_select", "audio"),
    ("commands::audio_settings", "audio"),
    ("commands::audio_set_guard", "audio"),
    ("commands::test_tone", "audio"),
    ("commands::synth_note", "audio"),
    ("commands::live_endpoint", "audio"),
    ("commands::audio_log", "audio"),
    ("project_cmds::channel_preview", "notes"),
    ("project_cmds::project_export", "export"),
    ("project_cmds::project_export_cancel", "export"),
    ("project_cmds::autosave_status", "recovery"),
    ("project_cmds::recovery_status", "recovery"),
    ("project_cmds::recovery_discard", "recovery"),
    ("project_cmds::app_exit", "shell"),
    ("patterns_cmds::pattern_list", "patterns"),
    ("patterns_cmds::pattern_edit", "patterns"),
    ("notes_cmds::pattern_notes", "binary"),
    ("notes_cmds::notes_edit", "notes"),
    ("notes_cmds::notes_undo", "notes"),
    ("notes_cmds::notes_redo", "notes"),
    ("notes_cmds::channel_note", "notes"),
    ("notes_cmds::channel_param_live", "notes"),
    ("notes_cmds::synth_meter", "notes"),
    ("notes_cmds::channel_select", "notes"),
    ("notes_cmds::channel_waveform", "binary"),
    ("notes_cmds::record_set", "notes"),
    ("notes_cmds::metronome_set", "notes"),
    ("midi_cmds::midi_inputs", "MIDI"),
    ("midi_cmds::midi_set_enabled", "MIDI"),
    ("sampler_trim_cmds::sampler_trim_reset", "sampler trim"),
    ("sampler_trim_cmds::sampler_trim_undo", "sampler trim"),
];

const FILE: &str = "contract.ts";

#[derive(Default)]
struct Declarations {
    types: BTreeMap<String, (std::any::TypeId, String)>,
    duplicate: Option<String>,
}

impl TypeVisitor for Declarations {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        if T::output_path().is_none() {
            return;
        }
        let config = Config::default();
        let name = T::ident(&config);
        let identity = std::any::TypeId::of::<T>();
        match self.types.entry(name) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert((identity, T::decl(&config)));
                T::visit_dependencies(self);
            }
            std::collections::btree_map::Entry::Occupied(slot) => {
                if slot.get().0 != identity {
                    self.duplicate = Some(slot.key().clone());
                }
            }
        }
    }
}

impl Declarations {
    fn reference<T: TS + 'static>(&mut self) -> String {
        self.visit::<T>();
        T::name(&Config::default())
    }

    fn write(self, output: &mut String) -> Result<(), String> {
        if let Some(name) = self.duplicate {
            return Err(format!("duplicate exported IPC DTO {name}"));
        }
        for (name, (_, declaration)) in self.types {
            if declaration
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|word| word == "bigint")
            {
                return Err(format!(
                    "{name} emits bigint; annotate the field with its JSON number range"
                ));
            }
            output.push_str("export ");
            for line in declaration.lines() {
                writeln!(output, "{}", line.trim_end()).unwrap();
            }
            output.push('\n');
        }
        Ok(())
    }
}

fn render(source: &Path) -> Result<String, String> {
    let registry = read(&source.join("lib.rs"))?;
    let mut declarations = Declarations::default();
    let commands = commands(&registry, MIGRATED, DEFERRED, &mut declarations, |module| {
        read(&source.join(format!("{module}.rs")))
    })?;
    let mut output = String::from("// Generated by make ipc-contract-update. Do not edit.\n\n");
    declarations.write(&mut output)?;

    output.push_str("export type CommandMap = {\n");
    for Command {
        name, signature, ..
    } in &commands
    {
        if let Some(signature) = signature {
            let args = if signature.args.is_empty() {
                "undefined".to_owned()
            } else {
                format!("{{ {} }}", signature.args.join("; "))
            };
            writeln!(
                output,
                "  {name}: {{ args: {args}; reply: {} }};",
                signature.reply
            )
            .unwrap();
        }
    }
    output.push_str("};\n\nexport type DeferredCommands = {\n");
    for Command { name, deferred, .. } in &commands {
        if let Some(reason) = deferred {
            writeln!(output, "  {name}: \"{reason}\";").unwrap();
        }
    }
    output.push_str("};\n");
    Ok(output.replace("\r\n", "\n"))
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn write_contract(directory: &Path, contract: &str) -> Result<(), String> {
    let path = directory.join(FILE);
    fs::write(&path, contract).map_err(|error| format!("{}: {error}", path.display()))
}

fn update_contract(directory: &Path, contract: &str) -> Result<(), String> {
    fs::create_dir_all(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    let existing = inventory(directory)?;
    // Prune case variants before writing on case-insensitive filesystems.
    for file in existing {
        if file != Path::new(FILE) {
            let path = directory.join(file);
            fs::remove_file(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        }
    }
    write_contract(directory, contract)
}

fn inventory(directory: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let entries =
        fs::read_dir(directory).map_err(|error| format!("{}: {error}", directory.display()))?;
    entries
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Err(format!(
                    "unexpected generated entry {}",
                    entry.path().display()
                ));
            }
            Ok(entry.file_name().into())
        })
        .collect()
}

fn compare(expected: &Path, committed: &Path) -> Result<(), String> {
    let generated = inventory(expected)?;
    if generated.is_empty() {
        return Err("generated IPC file inventory is empty".into());
    }
    let actual = inventory(committed)?;
    if actual != generated {
        let missing: Vec<_> = generated.difference(&actual).collect();
        let extra: Vec<_> = actual.difference(&generated).collect();
        return Err(format!(
            "IPC file inventory differs (missing: {missing:?}; extra: {extra:?})"
        ));
    }
    for file in generated {
        if read(&expected.join(&file))? != read(&committed.join(&file))?.replace("\r\n", "\n") {
            return Err(format!("IPC contract differs in {}", file.display()));
        }
    }
    Ok(())
}

fn paths() -> (PathBuf, PathBuf) {
    let app = Path::new(env!("CARGO_MANIFEST_DIR"));
    (app.join("src"), app.join("../ui/src/api/generated"))
}

pub fn update() -> Result<(), String> {
    let (source, committed) = paths();
    update_contract(&committed, &render(&source)?)
}

pub fn check() -> Result<(), String> {
    let (source, committed) = paths();
    let temp = tempfile::tempdir().map_err(|error| error.to_string())?;
    write_contract(temp.path(), &render(&source)?)?;
    compare(temp.path(), &committed)
        .map_err(|error| format!("{error}. Run make ipc-contract-update and review the diff."))
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wide;

#[cfg(test)]
#[test]
fn committed_ipc_contract_matches_rust() {
    check().unwrap();
}
