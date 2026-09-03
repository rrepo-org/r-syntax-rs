//! Durable corpus acquisition and differential parser testing.
#![forbid(unsafe_code)]

pub mod collect;
pub mod diff;
pub mod manifest;
pub mod minimize;
pub mod model;
pub mod oracle;
pub mod report;
pub mod run;
pub mod store;
pub mod task;
pub mod worker;

pub use model::{CorpusError, Result};
