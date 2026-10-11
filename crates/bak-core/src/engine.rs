//! The `Engine` trait: the only way `bak-cli` touches a backup repository.
//!
//! Each call opens the repository afresh. That keeps the trait free of
//! library-specific handle types at the cost of re-reading keys and config per
//! call, which is cheap next to any real operation.

use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::error::Result;
use crate::redact::Secret;
use crate::remote::RemoteSpec;

pub trait Engine {
    /// Whether a repository already exists at the remote.
    fn exists(&self, remote: &RemoteSpec) -> Result<bool>;

    /// Create a repository at the remote.
    fn init(&self, remote: &RemoteSpec, passphrase: &Secret, opts: &InitOptions) -> Result<()>;

    /// Snapshot `req.paths` (relative to `req.root`).
    fn snapshot(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        req: &SnapshotRequest<'_>,
    ) -> Result<PushSummary>;

    /// All snapshots, oldest first.
    fn snapshots(&self, remote: &RemoteSpec, passphrase: &Secret) -> Result<Vec<SnapshotInfo>>;

    /// Entries of a snapshot under `path` (the whole snapshot when `None`),
    /// recursively, in path order. Paths are relative to the snapshot root.
    fn entries(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        snapshot: &SnapshotRef,
        path: Option<&Path>,
    ) -> Result<Vec<Entry>>;

    /// Restore a snapshot, or the sub-path `path` of it, into `dest`. A sub-path
    /// is restored at the same relative location under `dest`.
    fn restore(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        snapshot: &SnapshotRef,
        path: Option<&Path>,
        dest: &Path,
    ) -> Result<RestoreSummary>;

    /// Check the repository structure, and with `read_data` every pack's content.
    fn verify(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        read_data: bool,
    ) -> Result<VerifyReport>;

    /// Copy snapshots (all when `snapshots` is empty) to another repository.
    fn copy(
        &self,
        from: &RemoteSpec,
        to: &RemoteSpec,
        passphrase: &Secret,
        snapshots: &[SnapshotRef],
    ) -> Result<CopySummary>;
}

#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// Target data pack size in MiB; the engine default when `None`.
    pub pack_size_mib: Option<u32>,
    /// Reuse the chunker parameters of this existing repository, so snapshots
    /// copied between the two deduplicate.
    pub chunker_from: Option<RemoteSpec>,
}

#[derive(Debug, Clone)]
pub struct SnapshotRequest<'a> {
    pub root: &'a Path,
    pub paths: &'a [String],
    /// Stable identity used to find the parent snapshot; the project name.
    pub host: &'a str,
    pub message: Option<&'a str>,
    pub tags: &'a [String],
    /// Ignore file name, in gitignore syntax.
    pub ignore_file: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotRef {
    Latest,
    Id(String),
}

impl SnapshotRef {
    pub fn parse(s: &str) -> Self {
        if s == "latest" {
            Self::Latest
        } else {
            Self::Id(s.to_string())
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Latest => "latest",
            Self::Id(id) => id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotInfo {
    pub id: String,
    pub short_id: String,
    /// RFC 3339 timestamp.
    pub time: String,
    pub host: String,
    pub message: Option<String>,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    /// Bytes of source data in the snapshot.
    pub size: u64,
    pub files: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PushSummary {
    pub snapshot: SnapshotInfo,
    pub files_new: u64,
    pub files_changed: u64,
    pub files_unmodified: u64,
    /// New data blobs written.
    pub data_blobs_added: u64,
    /// New bytes written before compression.
    pub bytes_added: u64,
    /// New bytes written after compression and encryption.
    pub bytes_added_stored: u64,
    pub seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Path relative to the snapshot root, `/`-separated. Non-UTF-8 bytes are
    /// shown lossily here; `raw_path` keeps the exact path.
    pub path: String,
    #[serde(skip)]
    pub raw_path: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    pub mode: Option<u32>,
    pub mtime: Option<String>,
    pub link_target: Option<String>,
    /// Fingerprint of the content; equal fingerprints mean equal content.
    pub content: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RestoreSummary {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyProblem {
    /// `true` for an error, `false` for a warning.
    pub error: bool,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct VerifyReport {
    pub problems: Vec<VerifyProblem>,
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        !self.problems.iter().any(|p| p.error)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CopySummary {
    pub copied: usize,
    pub already_present: usize,
}

/// What a progress display is counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressKind {
    Spinner,
    Counter,
    Bytes,
}

/// Progress display supplied by the front end.
pub trait ProgressReporter: Debug + Send + Sync + 'static {
    fn start(&self, kind: ProgressKind, title: &str) -> Arc<dyn ProgressTask>;
}

pub trait ProgressTask: Debug + Send + Sync + 'static {
    fn set_length(&self, len: u64);
    fn set_title(&self, title: &str);
    fn inc(&self, n: u64);
    fn finish(&self);
}

/// A reporter that shows nothing.
#[derive(Debug, Default)]
pub struct NoProgress;

#[derive(Debug)]
struct NoTask;

impl ProgressTask for NoTask {
    fn set_length(&self, _: u64) {}
    fn set_title(&self, _: &str) {}
    fn inc(&self, _: u64) {}
    fn finish(&self) {}
}

impl ProgressReporter for NoProgress {
    fn start(&self, _: ProgressKind, _: &str) -> Arc<dyn ProgressTask> {
        Arc::new(NoTask)
    }
}
