//! `Engine` implementation on top of `rustic_core` and `rustic_backend`.
//!
//! This is the only crate that names a rustic type. Every rustic error is
//! converted to [`bak_core::Error::Engine`], which scrubs registered secrets.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bak_core::engine::NoProgress;
use bak_core::{
    CopySummary, Engine, Entry, EntryKind, Error, InitOptions, Location, ProgressKind,
    ProgressReporter, ProgressTask, PushSummary, RemoteSpec, RestoreSummary, Result, Secret,
    SnapshotInfo, SnapshotRef, SnapshotRequest, VerifyProblem, VerifyReport,
};
use rustic_backend::{LocalBackend, RestBackend};
use rustic_core::repofile::{ConfigFile, Node, NodeType, SnapshotFile};
use rustic_core::{
    BackupOptions, CheckOptions, ConfigOptions, Credentials, Id, KeyOptions, LocalDestination,
    LsOptions, PathList, Progress, ProgressBars, ProgressType, Repository, RepositoryBackends,
    RepositoryOptions, RestoreOptions, RusticProgress, SnapshotOptions, WriteBackend,
};

/// Engine backed by rustic_core.
#[derive(Debug, Clone)]
pub struct RusticEngine {
    progress: Arc<dyn ProgressReporter>,
    cache_dir: Option<PathBuf>,
}

impl Default for RusticEngine {
    fn default() -> Self {
        Self::new(Arc::new(NoProgress), None)
    }
}

impl RusticEngine {
    /// `cache_dir` overrides rustic's default cache location (the platform cache dir).
    pub fn new(progress: Arc<dyn ProgressReporter>, cache_dir: Option<PathBuf>) -> Self {
        Self {
            progress,
            cache_dir,
        }
    }

    fn repository(&self, remote: &RemoteSpec) -> Result<Repository<()>> {
        let backend = backend(remote)?;
        let backends = RepositoryBackends::new(backend, None);
        let mut opts = RepositoryOptions::default();
        opts.cache_dir.clone_from(&self.cache_dir);
        Repository::new_with_progress(&opts, &backends, Bars(self.progress.clone())).map_err(err)
    }

    fn open(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
    ) -> Result<Repository<rustic_core::OpenStatus>> {
        self.repository(remote)?
            .open(&Credentials::password(passphrase.expose()))
            .map_err(err)
    }
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::engine(e)
}

fn backend(remote: &RemoteSpec) -> Result<Arc<dyn WriteBackend>> {
    match &remote.location {
        Location::Local(path) => {
            let path = path
                .to_str()
                .ok_or_else(|| Error::NonUtf8Path(path.clone()))?;
            Ok(Arc::new(LocalBackend::new(path, []).map_err(err)?))
        }
        Location::Rest { url } => {
            let url = match &remote.credentials {
                Some(c) => with_userinfo(url, &c.username, c.password.expose()),
                None => url.clone(),
            };
            let mut options = Vec::new();
            if let Some(ca) = &remote.cacert {
                options.push(("cacert".to_string(), ca.to_string_lossy().into_owned()));
            }
            Ok(Arc::new(RestBackend::new(url, options).map_err(err)?))
        }
    }
}

/// Put `user:password@` into an `http(s)://` URL. The result only ever lives
/// in memory and is handed straight to the REST backend.
fn with_userinfo(url: &str, user: &str, password: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    let enc = bak_core::redact::percent_encode_userinfo;
    format!("{scheme}://{}:{}@{rest}", enc(user), enc(password))
}

impl Engine for RusticEngine {
    fn exists(&self, remote: &RemoteSpec) -> Result<bool> {
        if let Location::Local(path) = &remote.location {
            if !path.join("config").exists() {
                return Ok(false);
            }
        }
        Ok(self.repository(remote)?.config_id().map_err(err)?.is_some())
    }

    fn init(&self, remote: &RemoteSpec, passphrase: &Secret, opts: &InitOptions) -> Result<()> {
        if let Location::Local(path) = &remote.location {
            std::fs::create_dir_all(path)
                .map_err(|e| Error::io(format!("cannot create {}", path.display()), e))?;
        }
        let creds = Credentials::password(passphrase.expose());
        let pack_size = opts
            .pack_size_mib
            .map(|m| bytesize::ByteSize::mib(u64::from(m)));
        let repo = self.repository(remote)?;
        if let Some(template) = &opts.chunker_from {
            let mut config = self.open(template, passphrase)?.config().clone();
            config = with_new_repository_id(&config)?;
            if let Some(size) = pack_size {
                config.datapack_size = Some(u32::try_from(size.as_u64()).map_err(err)?);
            }
            repo.init_with_config(&creds, &KeyOptions::default(), config)
                .map_err(err)?;
        } else {
            let mut config_opts = ConfigOptions::default();
            config_opts.set_datapack_size = pack_size;
            repo.init(&creds, &KeyOptions::default(), &config_opts)
                .map_err(err)?;
        }
        Ok(())
    }

    fn snapshot(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        req: &SnapshotRequest<'_>,
    ) -> Result<PushSummary> {
        // rustic records source paths exactly as given and resolves relative
        // ones against the process working directory, so the project root
        // must become the working directory for relative snapshot paths.
        std::env::set_current_dir(req.root)
            .map_err(|e| Error::io(format!("cannot enter {}", req.root.display()), e))?;

        let repo = self
            .open(remote, passphrase)?
            .to_indexed_ids()
            .map_err(err)?;

        let mut snap_opts = SnapshotOptions::default();
        snap_opts.host = Some(req.host.to_string());
        snap_opts.description = req.message.map(str::to_string);
        if !req.tags.is_empty() {
            snap_opts = snap_opts.add_tags(&req.tags.join(",")).map_err(err)?;
        }
        let snap = snap_opts.to_snapshot().map_err(err)?;

        let mut backup_opts = BackupOptions::default();
        backup_opts.ignore_filter_opts.custom_ignorefiles = vec![req.ignore_file.to_string()];

        let source = PathList::from_iter(req.paths.iter().map(PathBuf::from))
            .sanitize()
            .map_err(err)?;
        let snap = repo.backup(&backup_opts, &source, snap).map_err(err)?;
        let s = snap.summary.clone().unwrap_or_default();
        Ok(PushSummary {
            snapshot: snapshot_info(&snap),
            files_new: s.files_new,
            files_changed: s.files_changed,
            files_unmodified: s.files_unmodified,
            data_blobs_added: s.data_blobs,
            bytes_added: s.data_added,
            bytes_added_stored: s.data_added_packed,
            seconds: s.total_duration,
        })
    }

    fn snapshots(&self, remote: &RemoteSpec, passphrase: &Secret) -> Result<Vec<SnapshotInfo>> {
        let repo = self.open(remote, passphrase)?;
        let mut snaps = repo.get_all_snapshots().map_err(err)?;
        snaps.sort_by(|a, b| a.time.cmp(&b.time));
        Ok(snaps.iter().map(snapshot_info).collect())
    }

    fn entries(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        snapshot: &SnapshotRef,
        path: Option<&Path>,
    ) -> Result<Vec<Entry>> {
        let repo = self
            .open(remote, passphrase)?
            .to_indexed_ids()
            .map_err(err)?;
        let snap = repo
            .get_snapshot_from_str(snapshot.as_str(), |_| true)
            .map_err(err)?;
        let sub = sub_path_str(path)?;
        let node = repo.node_from_snapshot_and_path(&snap, &sub).map_err(err)?;
        let base = PathBuf::from(&sub);
        if !node.is_dir() {
            return Ok(vec![entry(base, &node)]);
        }
        let mut out = Vec::new();
        for item in repo.ls(&node, &LsOptions::default()).map_err(err)? {
            let (p, node) = item.map_err(err)?;
            out.push(entry(base.join(p), &node));
        }
        Ok(out)
    }

    fn restore(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        snapshot: &SnapshotRef,
        path: Option<&Path>,
        dest: &Path,
    ) -> Result<RestoreSummary> {
        let repo = self.open(remote, passphrase)?.to_indexed().map_err(err)?;
        let snap = repo
            .get_snapshot_from_str(snapshot.as_str(), |_| true)
            .map_err(err)?;
        let sub = sub_path_str(path)?;
        let node = repo.node_from_snapshot_and_path(&snap, &sub).map_err(err)?;
        let target = if sub.is_empty() {
            dest.to_path_buf()
        } else {
            dest.join(&sub)
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::io(format!("cannot create {}", parent.display()), e))?;
        }
        let target_str = target
            .to_str()
            .ok_or_else(|| Error::NonUtf8Path(target.clone()))?;
        let dest = LocalDestination::new(target_str, true, !node.is_dir()).map_err(err)?;

        let mut opts = RestoreOptions::default();
        // Restoring ownership needs root; v0.1 restores content, modes and times only.
        opts.no_ownership = true;
        let ls = repo.ls(&node, &LsOptions::default()).map_err(err)?;
        let plan = repo
            .prepare_restore(&opts, ls.clone(), &dest, false)
            .map_err(err)?;
        let summary = RestoreSummary {
            files: plan.stats.files.restore,
            dirs: plan.stats.dirs.restore,
            bytes: plan.restore_size,
        };
        repo.restore(plan, &opts, ls, &dest).map_err(err)?;
        Ok(summary)
    }

    fn verify(
        &self,
        remote: &RemoteSpec,
        passphrase: &Secret,
        read_data: bool,
    ) -> Result<VerifyReport> {
        let repo = self.open(remote, passphrase)?;
        let results = repo
            .check(CheckOptions::default().read_data(read_data))
            .map_err(err)?;
        let problems = results
            .0
            .iter()
            .map(|(level, e)| VerifyProblem {
                // CheckErrorLevel is public in CheckResults but not re-exported,
                // so it can only be told apart through its Debug output.
                error: format!("{level:?}") == "Error",
                message: bak_core::redact::scrub(&e.to_string()),
            })
            .collect();
        Ok(VerifyReport { problems })
    }

    fn copy(
        &self,
        from: &RemoteSpec,
        to: &RemoteSpec,
        passphrase: &Secret,
        snapshots: &[SnapshotRef],
    ) -> Result<CopySummary> {
        let src = self.open(from, passphrase)?.to_indexed().map_err(err)?;
        let dest = self.open(to, passphrase)?.to_indexed_ids().map_err(err)?;
        let snaps = if snapshots.is_empty() {
            src.get_all_snapshots().map_err(err)?
        } else {
            snapshots
                .iter()
                .map(|s| src.get_snapshot_from_str(s.as_str(), |_| true))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(err)?
        };
        let candidates = dest
            .relevant_copy_snapshots(|_| true, &snaps)
            .map_err(err)?;
        let relevant: Vec<&SnapshotFile> = candidates
            .iter()
            .filter(|c| c.relevant)
            .map(|c| &c.sn)
            .collect();
        let summary = CopySummary {
            copied: relevant.len(),
            already_present: candidates.len() - relevant.len(),
        };
        src.copy(&dest, relevant).map_err(err)?;
        Ok(summary)
    }
}

/// `ConfigFile` keeps its repository id type private, so a config can only
/// be given a fresh id by round-tripping it through serde.
fn with_new_repository_id(config: &ConfigFile) -> Result<ConfigFile> {
    let mut value = serde_json::to_value(config).map_err(err)?;
    value["id"] = serde_json::Value::String(Id::random().to_hex().to_string());
    serde_json::from_value(value).map_err(err)
}

fn sub_path_str(path: Option<&Path>) -> Result<String> {
    let Some(path) = path else {
        return Ok(String::new());
    };
    let s = path
        .to_str()
        .ok_or_else(|| Error::NonUtf8Path(path.to_path_buf()))?;
    Ok(s.trim_matches('/').to_string())
}

fn snapshot_info(snap: &SnapshotFile) -> SnapshotInfo {
    let id = snap.id.to_string();
    let summary = snap.summary.clone().unwrap_or_default();
    SnapshotInfo {
        short_id: id.chars().take(8).collect(),
        id,
        time: snap.time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        host: snap.hostname.clone(),
        message: snap.description.clone(),
        tags: snap.tags.iter().cloned().collect(),
        paths: snap.paths.iter().cloned().collect(),
        size: summary.total_bytes_processed,
        files: summary.total_files_processed,
    }
}

fn entry(path: PathBuf, node: &Node) -> Entry {
    let (kind, link_target) = match &node.node_type {
        NodeType::File => (EntryKind::File, None),
        NodeType::Dir => (EntryKind::Dir, None),
        NodeType::Symlink { .. } => (
            EntryKind::Symlink,
            Some(node.node_type.to_link().to_string_lossy().into_owned()),
        ),
        _ => (EntryKind::Other, None),
    };
    let content = node.content.as_ref().map(|ids| {
        // FNV-1a over the blob ids: equal content gives equal blob lists.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for id in ids {
            for b in id.to_string().bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{h:016x}")
    });
    Entry {
        path: path.to_string_lossy().into_owned(),
        raw_path: path,
        kind,
        size: node.meta.size,
        mode: node.meta.mode,
        mtime: node.meta.mtime.map(|t| t.to_string()),
        link_target,
        content,
    }
}

/// Adapts the front end's progress reporter to rustic's progress traits.
#[derive(Debug, Clone)]
struct Bars(Arc<dyn ProgressReporter>);

impl ProgressBars for Bars {
    fn progress(&self, progress_type: ProgressType, prefix: &str) -> Progress {
        let kind = match progress_type {
            ProgressType::Bytes => ProgressKind::Bytes,
            ProgressType::Counter => ProgressKind::Counter,
            _ => ProgressKind::Spinner,
        };
        Progress::new(Task(self.0.start(kind, prefix)))
    }
}

#[derive(Debug)]
struct Task(Arc<dyn ProgressTask>);

impl RusticProgress for Task {
    fn is_hidden(&self) -> bool {
        false
    }
    fn set_length(&self, len: u64) {
        self.0.set_length(len);
    }
    fn set_title(&self, title: &str) {
        self.0.set_title(title);
    }
    fn inc(&self, inc: u64) {
        self.0.inc(inc);
    }
    fn finish(&self) {
        self.0.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn userinfo_is_encoded() {
        assert_eq!(
            with_userinfo("https://h:8000/p", "u", "p@ss"),
            "https://u:p%40ss@h:8000/p"
        );
    }
}
