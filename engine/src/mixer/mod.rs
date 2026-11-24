//! Control-thread-only routing descriptors and deterministic admission/compilation.
//! No devices are constructed and no live graph is changed by this module.
//! This orders explicit mixer inputs only; legacy source reduction/identity bypass
//! remains a separate runtime contract.
mod compile;
mod model;
pub use compile::*;
pub use model::*;
