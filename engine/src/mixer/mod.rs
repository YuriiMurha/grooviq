//! Routing descriptors, deterministic admission/compilation, and a static serial renderer.
//! Descriptors and compilation are control-thread only and construct no devices. The
//! renderer takes caller-prepared devices and caller-resolved source blocks; only its
//! `render`, `reset` and `set_param` run on the audio thread. No live graph is changed here,
//! and legacy source reduction/identity bypass remains a separate runtime contract.
mod compile;
mod insert;
mod model;
mod runtime;
pub use compile::*;
pub use insert::{ParamError, PreparedInsert};
pub use model::*;
pub use runtime::*;
