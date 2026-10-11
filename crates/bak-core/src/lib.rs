//! Engine-neutral core of `bak`: project config, remotes, the mount guard,
//! secret redaction, snapshot diffing and the [`Engine`] trait.
//!
//! Nothing in this crate's public API names a type from a backup library.

pub mod config;
pub mod diff;
pub mod engine;
pub mod error;
pub mod mount;
pub mod redact;
pub mod remote;

pub use config::{Config, Project, RemoteConfig};
pub use engine::{
    CopySummary, Engine, Entry, EntryKind, InitOptions, ProgressKind, ProgressReporter,
    ProgressTask, PushSummary, RestoreSummary, SnapshotInfo, SnapshotRef, SnapshotRequest,
    VerifyProblem, VerifyReport,
};
pub use error::{Error, Result};
pub use redact::Secret;
pub use remote::{Location, RemoteSpec, RestCredentials};
