//! Routing descriptors, deterministic admission/compilation, a static serial renderer and
//! the live install (`live`). Descriptors and compilation are control-thread only and
//! construct no devices. The renderer takes caller-prepared devices and caller-resolved
//! or pulled source blocks; only its `render*`, `reset`, `set_param` and `transfer_from` run
//! on the audio thread. LegacyDirect stays a separate graph path: a routed canonical sum is
//! not claimed to reproduce the legacy addition order.
mod compile;
mod insert;
mod latency;
pub(crate) mod live;
mod model;
mod runtime;
pub use compile::*;
pub use insert::{InsertIdentity, ParamError, PreparedInsert};
pub use latency::*;
pub use live::{
    InsertDevice, MixSetup, RoutedMix, UnsupportedInsertRate, describe_builtin_insert, insert_kinds,
};
pub use model::*;
pub use runtime::*;
