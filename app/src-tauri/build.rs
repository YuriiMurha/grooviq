#[cfg(feature = "ipc-contract")]
#[path = "src/ipc_contract/wide.rs"]
mod wide;

fn main() {
    #[cfg(feature = "ipc-contract")]
    {
        let app = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut sources = Vec::new();
        read_sources(&app.join("src"), &mut sources).expect("read app IPC sources");
        read_sources(&app.join("../../project/src"), &mut sources)
            .expect("read project IPC sources");
        wide::validate_sources(
            sources
                .iter()
                .map(|(path, source)| (path.as_str(), source.as_str())),
        )
        .expect("IPC JSON integer annotations");
    }
    tauri_build::build();
}

#[cfg(feature = "ipc-contract")]
fn read_sources(
    directory: &std::path::Path,
    sources: &mut Vec<(String, String)>,
) -> std::io::Result<()> {
    use std::fs;

    println!("cargo:rerun-if-changed={}", directory.display());
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            read_sources(&path, sources)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push((path.display().to_string(), fs::read_to_string(&path)?));
        }
    }
    Ok(())
}
