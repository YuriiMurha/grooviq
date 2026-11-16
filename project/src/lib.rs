//! The Grooviq project: the model, the `.grooviq` file and the step from a project to the
//! engine's state.
//!
//! * [`model`]: the project as data (serde, camelCase, unknown fields ignored) and its ranges.
//! * [`file`]: the `.grooviq` zip (a versioned `project.json` plus every sample's original
//!   bytes), atomic save, load with migrations.
//! * [`compile`]: swing, sorting and the engine's [`EngineState`](grooviq_engine::EngineState).
//! * [`backup`]: rolling copies of a file before it is overwritten.
//!
//! The crate works on bytes: it never decodes a sample, so it needs no audio code to run.

pub mod backup;
pub mod compile;
pub mod file;
pub mod model;

pub use backup::backup_before_save;
pub use compile::{compile_pattern, engine_state, sampler_values};
#[doc(hidden)]
pub use file::save_with;
pub use file::{LoadError, Loaded, SampleFile, SampleFiles, load, migrate, read_sample_file, save};
pub use model::{
    Channel, DEFAULT_KEY, DEFAULT_VELOCITY, EXTENSION, FORMAT, MAX_STEPS, Note, Pattern, Project,
    SampleRef, TimeSignature, VERSION,
};
