//! The project commands: edits, channels, transport, open, save, export, recovery and exit.
//!
//! Each is a thin call into the [`Session`]. Reading, decoding, saving and exporting run on a
//! blocking thread and never under the session's lock: the lock is taken only to snapshot
//! before and to apply the result after. Lock order: session, then controller (inside the
//! session's own calls); nothing takes the session's lock while holding the controller's.

use crate::autosave_notice::AutosaveStatus;
use crate::{Audio, lock};
use grooviq_engine::{
    Command, ExportOptions, MAX_SAMPLE_SECONDS, SampleBuf, WavFormat, decode_sample, export_wav,
};
use grooviq_project::recovery::Recovery;
use grooviq_project::session::{OpenReport, ProjectView, SaveJob, Session};
use grooviq_project::{SampleFile, decode_samples, load, read_sample_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};

/// The event an export's progress goes out on: a number 0..1.
pub(crate) const EXPORT_PROGRESS: &str = "export-progress";
/// The quietest peak an export reports, in dB (a silent file).
const PEAK_FLOOR_DB: f32 = -120.0;
/// The longest tail after the last loop of an export, in seconds.
const EXPORT_TAIL_SECONDS: f32 = 10.0;
/// The most loops one export may play.
const MAX_EXPORT_LOOPS: u32 = 999;
/// Export rates the engine runs at.
const EXPORT_RATES: (u32, u32) = (8_000, 384_000);

/// [`Session::open`]'s report, as the UI gets it: `{ view, warnings }`.
pub type OpenResult = OpenReport;

/// A recovered project waiting for the person's choice.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryView {
    /// When it was autosaved, in unix seconds.
    saved_at: u64,
    /// The file it was, or `null` if it was untitled.
    original: Option<String>,
}

/// What `project_export` takes.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    sample_rate: u32,
    format: ExportFormat,
    loops: u32,
}

/// `"pcm24"` or `"float32"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExportFormat {
    Pcm24,
    Float32,
}

/// What `project_export` wrote.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    frames: u64,
    seconds: f64,
    /// The loudest sample in dB full scale (above 0 if it clipped); -120 for silence.
    peak_db: f32,
    /// Samples above full scale (left and right counted apart).
    clipped: u64,
}

impl ExportRequest {
    fn options(self) -> Result<ExportOptions, String> {
        if !(EXPORT_RATES.0..=EXPORT_RATES.1).contains(&self.sample_rate) {
            return Err(format!(
                "The sample rate must be {} Hz to {} Hz.",
                EXPORT_RATES.0, EXPORT_RATES.1
            ));
        }
        if !(1..=MAX_EXPORT_LOOPS).contains(&self.loops) {
            return Err(format!(
                "The pattern can play 1 to {MAX_EXPORT_LOOPS} times."
            ));
        }
        Ok(ExportOptions {
            sample_rate: self.sample_rate,
            format: match self.format {
                ExportFormat::Pcm24 => WavFormat::Pcm24,
                ExportFormat::Float32 => WavFormat::Float32,
            },
            loops: self.loops,
            max_tail_seconds: EXPORT_TAIL_SECONDS,
        })
    }
}

#[allow(clippy::disallowed_methods, reason = "display only")]
fn peak_db(peak: f32) -> f32 {
    if peak > 0.0 && peak.is_finite() {
        (20.0 * peak.log10()).max(PEAK_FLOOR_DB)
    } else {
        PEAK_FLOOR_DB
    }
}

/// `path`, with `.extension` added unless it already ends in it (any case).
fn with_extension(path: &Path, extension: &str) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
    {
        return path.to_path_buf();
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

/// Passes progress on only when it moved by a percent, or reached the end, so a fast export
/// does not flood the UI.
struct Throttle {
    last: f32,
}

impl Throttle {
    fn pass(&mut self, progress: f32) -> bool {
        if progress >= 1.0 || progress - self.last >= 0.01 {
            self.last = progress;
            return true;
        }
        false
    }
}

/// Runs `work` on a blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("The work stopped unexpectedly: {e}"))?
}

/// Reads and decodes a sample file the person picked.
fn read_and_decode(path: &Path) -> Result<(SampleFile, Arc<SampleBuf>), String> {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let file = read_sample_file(path).map_err(|e| format!("\"{name}\" could not be read: {e}."))?;
    let extension = path.extension().and_then(|e| e.to_str());
    let buf = decode_sample(&file.bytes, extension, MAX_SAMPLE_SECONDS)
        .map_err(|e| format!("\"{name}\": {e}"))?;
    Ok((file, Arc::new(buf)))
}

fn session(audio: &Audio) -> std::sync::MutexGuard<'_, Session> {
    lock(&audio.session)
}

/// The project as the UI draws it.
#[tauri::command]
pub async fn project_view(audio: State<'_, Audio>) -> Result<ProjectView, String> {
    Ok(session(&audio).view())
}

/// Applies one edit.
#[tauri::command]
pub async fn project_edit(
    audio: State<'_, Audio>,
    edit: grooviq_project::session::Edit,
) -> Result<ProjectView, String> {
    session(&audio)
        .apply(edit, Instant::now())
        .map_err(|e| e.to_string())
}

/// Adds a channel playing the sample file at `path`.
#[tauri::command]
pub async fn channel_add(audio: State<'_, Audio>, path: String) -> Result<ProjectView, String> {
    let path = PathBuf::from(path);
    let (file, buf) = blocking({
        let path = path.clone();
        move || read_and_decode(&path)
    })
    .await?;
    session(&audio)
        .add_channel(&path, file, buf, Instant::now())
        .map_err(|e| e.to_string())
}

/// Gives `channel` the sample file at `path` (relinking a missing sample, or replacing it).
#[tauri::command]
pub async fn channel_set_sample(
    audio: State<'_, Audio>,
    channel: u32,
    path: String,
) -> Result<ProjectView, String> {
    let path = PathBuf::from(path);
    let (file, buf) = blocking({
        let path = path.clone();
        move || read_and_decode(&path)
    })
    .await?;
    session(&audio)
        .set_channel_sample(channel, &path, file, buf, Instant::now())
        .map_err(|e| e.to_string())
}

/// Starts or releases a preview of `channel`.
#[tauri::command]
pub async fn channel_preview(
    audio: State<'_, Audio>,
    channel: u32,
    on: bool,
) -> Result<(), String> {
    session(&audio)
        .preview(channel, on)
        .map_err(|e| e.to_string())
}

fn transport(audio: &Audio, cmd: Command) -> Result<(), String> {
    lock(&audio.controller)
        .send(cmd)
        .map_err(|_| "The engine is busy; try again.".to_owned())
}

/// Plays the pattern from its start, looping.
#[tauri::command]
pub async fn transport_play(audio: State<'_, Audio>) -> Result<(), String> {
    transport(&audio, Command::Play)
}

/// Stops and rewinds.
#[tauri::command]
pub async fn transport_stop(audio: State<'_, Audio>) -> Result<(), String> {
    transport(&audio, Command::Stop)
}

/// A project was replaced (new, opened, restored): the autosave on disk is the old one's, and
/// a crash now must not offer it back. One the folder will not drop is told in the log; the
/// next autosave of the open project replaces it.
fn forget_autosave(audio: &Audio) {
    if let Some(dir) = lock(&audio.recovery).as_ref()
        && let Err(e) = dir.clear_autosave()
    {
        lock(&audio.log).push(
            crate::audio_log::Level::Warn,
            format!("The previous project's autosave could not be removed: {e}"),
        );
    }
}

/// Starts an empty, untitled project. The UI asks about unsaved changes first.
#[tauri::command]
pub async fn project_new(audio: State<'_, Audio>) -> Result<ProjectView, String> {
    let view = session(&audio).new_project().map_err(|e| e.to_string())?;
    forget_autosave(&audio);
    Ok(view)
}

/// Opens the project at `path`. The UI asks about unsaved changes first.
#[tauri::command]
pub async fn project_open(audio: State<'_, Audio>, path: String) -> Result<OpenResult, String> {
    let path = PathBuf::from(path);
    let (loaded, decoded) = blocking({
        let path = path.clone();
        move || {
            let loaded = load(&path).map_err(|e| e.to_string())?;
            let decoded = decode_samples(&loaded);
            Ok((loaded, decoded))
        }
    })
    .await?;
    let report = session(&audio)
        .open(&path, loaded, decoded)
        .map_err(|e| e.to_string())?;
    forget_autosave(&audio);
    Ok(report)
}

/// Saves where the project lives (fails with "Choose where to save this project." while
/// untitled: the UI then asks for a path and calls `project_save_as`).
#[tauri::command]
pub async fn project_save(audio: State<'_, Audio>) -> Result<ProjectView, String> {
    save_to(&audio, None).await
}

/// Saves to `path` (`.grooviq` is added if missing), which becomes the project's file.
#[tauri::command]
pub async fn project_save_as(audio: State<'_, Audio>, path: String) -> Result<ProjectView, String> {
    save_to(&audio, Some(PathBuf::from(path))).await
}

async fn save_to(audio: &Audio, path: Option<PathBuf>) -> Result<ProjectView, String> {
    let job: SaveJob = session(audio)
        .begin_save(path.as_deref())
        .map_err(|e| e.to_string())?;
    let backups = audio.backups.clone();
    let written = blocking(move || {
        job.write(&backups).map_err(|e| e.to_string())?;
        Ok(job)
    })
    .await;
    let (view, clean) = {
        let mut session = session(audio);
        let job = match written {
            Ok(job) => job,
            Err(e) => {
                session.save_failed();
                return Err(e);
            }
        };
        // False if another project was opened or started while the file was written: that
        // file holds the old project, and the open one keeps its own path.
        let applied = session.finish_save(&job);
        (session.view(), applied && !session.needs_confirmation())
    };
    if clean && let Some(dir) = lock(&audio.recovery).as_ref() {
        // Nothing unsaved is left to recover. A stale autosave is only an older copy.
        let _ = dir.clear_autosave();
    }
    Ok(view)
}

/// Exports the project as it plays now to a WAV file at `path` (`.wav` is added if missing),
/// emitting `export-progress`. One export at a time.
#[tauri::command]
pub async fn project_export(
    app: AppHandle,
    audio: State<'_, Audio>,
    path: String,
    options: ExportRequest,
) -> Result<ExportResult, String> {
    let options = options.options()?;
    let path = with_extension(Path::new(&path), "wav");
    if audio.export_running.swap(true, Ordering::AcqRel) {
        return Err("An export is already running.".into());
    }
    audio.export_cancel.store(false, Ordering::Release);
    let state = session(&audio).export_state();
    let cancel = Arc::clone(&audio.export_cancel);
    let result = blocking(move || {
        let mut throttle = Throttle { last: -1.0 };
        let mut progress = |p: f32| {
            if throttle.pass(p) {
                // Fails only while the app shuts down.
                let _ = app.emit(EXPORT_PROGRESS, p);
            }
        };
        export_wav(state, &options, &path, &mut progress, &cancel).map_err(|e| e.to_string())
    })
    .await;
    audio.export_running.store(false, Ordering::Release);
    let report = result?;
    Ok(ExportResult {
        frames: report.frames,
        seconds: report.frames as f64 / f64::from(options.sample_rate),
        peak_db: peak_db(report.peak),
        clipped: report.clipped,
    })
}

/// Asks a running export to stop (it removes its partial file).
#[tauri::command]
pub async fn project_export_cancel(audio: State<'_, Audio>) -> Result<(), String> {
    audio.export_cancel.store(true, Ordering::Release);
    Ok(())
}

/// What the person is owed about autosave: `{ disabled, failure }`, sentences or `null`. The
/// UI asks once its listeners are attached: a notice raised before that is kept for it here
/// (`autosave-failed` carries later failures only).
#[tauri::command]
pub async fn autosave_status(audio: State<'_, Audio>) -> Result<AutosaveStatus, String> {
    Ok(lock(&audio.autosave).clone())
}

/// The project the last session left unsaved, if any.
#[tauri::command]
pub async fn recovery_status(audio: State<'_, Audio>) -> Result<Option<RecoveryView>, String> {
    Ok(lock(&audio.pending_recovery)
        .as_ref()
        .map(|r: &Recovery| RecoveryView {
            saved_at: r.saved_at,
            original: r
                .original
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        }))
}

/// Opens the recovered project at its original path, unsaved.
#[tauri::command]
pub async fn recovery_restore(audio: State<'_, Audio>) -> Result<OpenResult, String> {
    let recovery = lock(&audio.pending_recovery)
        .clone()
        .ok_or("There is no project to recover.")?;
    let (loaded, decoded) = blocking({
        let file = recovery.file.clone();
        move || {
            let loaded = load(&file).map_err(|e| e.to_string())?;
            let decoded = decode_samples(&loaded);
            Ok((loaded, decoded))
        }
    })
    .await?;
    let report = session(&audio)
        .open_recovered(
            recovery.original.as_deref(),
            loaded,
            decoded,
            Instant::now(),
        )
        .map_err(|e| e.to_string())?;
    forget_autosave(&audio);
    lock(&audio.pending_recovery).take();
    // No longer offered. If the folder refuses, it is offered again next launch: not worth
    // failing the restore over.
    if let Some(dir) = lock(&audio.recovery).as_ref()
        && let Err(e) = dir.restored(&recovery)
    {
        lock(&audio.log).push(
            crate::audio_log::Level::Warn,
            format!("The restored project stays offered on the next launch: {e}"),
        );
    }
    Ok(report)
}

/// Deletes the recovered project.
#[tauri::command]
pub async fn recovery_discard(audio: State<'_, Audio>) -> Result<(), String> {
    let Some(recovery) = lock(&audio.pending_recovery).take() else {
        return Ok(());
    };
    match lock(&audio.recovery).as_ref() {
        Some(dir) => dir
            .discard(recovery)
            .map_err(|e| format!("The recovered project could not be deleted: {e}.")),
        None => Ok(()),
    }
}

/// Quits after the person chose (saved, or chose to lose the changes): a clean exit, then the
/// app ends.
#[tauri::command]
pub async fn app_exit(app: AppHandle, audio: State<'_, Audio>) -> Result<(), String> {
    if let Err(e) = crate::exit::confirm_exit(&audio.exit, &audio.recovery) {
        lock(&audio.log).push(
            crate::audio_log::Level::Warn,
            format!("The recovery folder was not cleaned: {e}"),
        );
    }
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_requests_have_the_json_shape_the_ui_sends() {
        let r: ExportRequest =
            serde_json::from_str(r#"{"sampleRate":44100,"format":"float32","loops":2}"#).unwrap();
        let o = r.options().unwrap();
        assert_eq!(
            (o.sample_rate, o.format, o.loops),
            (44_100, WavFormat::Float32, 2)
        );
        let r: ExportRequest =
            serde_json::from_str(r#"{"sampleRate":48000,"format":"pcm24","loops":1}"#).unwrap();
        assert_eq!(r.options().unwrap().format, WavFormat::Pcm24);
        assert!(
            serde_json::from_str::<ExportRequest>(r#"{"sampleRate":1,"format":"mp3","loops":1}"#)
                .is_err()
        );
    }

    #[test]
    fn export_requests_out_of_range_are_refused() {
        let req = |sample_rate, loops| ExportRequest {
            sample_rate,
            format: ExportFormat::Pcm24,
            loops,
        };
        assert!(req(7_999, 1).options().is_err());
        assert!(req(384_001, 1).options().is_err());
        assert!(req(48_000, 0).options().is_err());
        assert!(req(48_000, 1_000).options().is_err());
        assert!(req(8_000, 999).options().is_ok());
        assert!(req(384_000, 1).options().is_ok());
    }

    #[test]
    fn the_result_has_the_json_shape_the_ui_reads() {
        let json = serde_json::to_value(ExportResult {
            frames: 48_000,
            seconds: 1.0,
            peak_db: -6.0,
            clipped: 0,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"frames": 48000, "seconds": 1.0, "peakDb": -6.0, "clipped": 0})
        );
        let json = serde_json::to_value(RecoveryView {
            saved_at: 5,
            original: None,
        })
        .unwrap();
        assert_eq!(json, serde_json::json!({"savedAt": 5, "original": null}));
    }

    #[test]
    fn peak_db_is_full_scale_relative_and_floored() {
        assert_eq!(peak_db(1.0), 0.0);
        assert!((peak_db(0.5) + 6.0206).abs() < 1e-3);
        assert!(peak_db(2.0) > 6.0);
        assert_eq!(peak_db(0.0), PEAK_FLOOR_DB);
        assert_eq!(peak_db(f32::NAN), PEAK_FLOOR_DB);
        assert_eq!(peak_db(1e-9), PEAK_FLOOR_DB);
    }

    #[test]
    fn the_extension_is_added_once() {
        assert_eq!(
            with_extension(Path::new("/a/mix"), "wav"),
            PathBuf::from("/a/mix.wav")
        );
        assert_eq!(
            with_extension(Path::new("/a/mix.WAV"), "wav"),
            PathBuf::from("/a/mix.WAV")
        );
        assert_eq!(
            with_extension(Path::new("/a/mix.v2"), "wav"),
            PathBuf::from("/a/mix.v2.wav")
        );
    }

    #[test]
    fn progress_is_passed_on_per_percent_and_at_the_end() {
        let mut t = Throttle { last: -1.0 };
        let passed: Vec<f32> = [0.0, 0.001, 0.005, 0.0101, 0.015, 0.5, 0.999, 1.0]
            .into_iter()
            .filter(|&p| t.pass(p))
            .collect();
        assert_eq!(passed, vec![0.0, 0.0101, 0.5, 0.999, 1.0]);
    }
}
