//! The Grooviq project: the model, the `.grooviq` file and the step from a project to the
//! engine's state.
//!
//! * [`model`]: the project as data (serde, camelCase, unknown fields ignored) and its ranges.
//! * [`file`]: the `.grooviq` zip (a versioned `project.json` plus every sample's original
//!   bytes), atomic save, load with migrations.
//! * [`compile`]: swing, sorting and the engine's [`EngineState`](grooviq_engine::EngineState).
//! * [`backup`]: rolling copies of a file before it is overwritten.
//! * [`session`]: the open project, its edits kept in step with the engine, dirty state and
//!   when to autosave.
//! * [`recovery`]: the autosave file and crash recovery on the next launch.
//! * [`autosave`]: one tick of the autosave clock, from the session to the recovery folder.
//!
//! The file code works on bytes and never decodes a sample; the session decodes embedded
//! samples ([`decode_samples`]) for the engine.

pub mod autosave;
pub mod backup;
pub mod compile;
pub mod file;
pub mod model;
mod outline;
pub mod recovery;
pub mod session;

pub use backup::{backup_before_save, backup_folder};
pub use compile::{compile_pattern, engine_state, sampler_values, synth_values};
#[doc(hidden)]
pub use file::save_with;
pub use file::{LoadError, Loaded, SampleFile, SampleFiles, load, migrate, read_sample_file, save};
pub use model::{
    Channel, DEFAULT_KEY, DEFAULT_VELOCITY, EXTENSION, FORMAT, MAX_NOTES_PER_PATTERN, MAX_STEPS,
    Note, Pattern, Project, ProjectKey, SCALES, SampleRef, TimeSignature, VERSION, is_plain_step,
    roll_channels,
};
pub use session::{DecodedSamples, decode_samples};
