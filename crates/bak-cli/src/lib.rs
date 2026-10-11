//! The `bak` command line. Commands only see `&dyn Engine`; `main.rs` picks the engine.

pub mod progress;

use std::cell::OnceCell;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use bak_core::config::IGNORE_FILE;
use bak_core::remote::{check_private_file, is_cloud_synced_folder, CLOUD_FOLDER_PACK_SIZE_MIB};
use bak_core::{
    diff, Engine, EntryKind, InitOptions, Location, Project, RemoteConfig, RemoteSpec, Secret,
    SnapshotInfo, SnapshotRef, SnapshotRequest,
};
use clap::{ArgAction, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "bak",
    version,
    about = "Versioned, encrypted backups of project folders"
)]
pub struct Cli {
    /// More output: -v for info, -vv for debug.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Read the repository passphrase from this file (mode 0600).
    #[arg(long, global = true, value_name = "FILE")]
    pub password_file: Option<PathBuf>,

    /// Machine-readable JSON output (status, log, ls, diff, push, verify).
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create .bak/config.toml in the current directory.
    Init,
    /// Track paths.
    Add {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Stop tracking paths. Touches no backup data.
    Untrack {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Show tracked paths, remotes and the latest snapshot per remote.
    Status,
    /// Manage remotes.
    #[command(subcommand)]
    Remote(RemoteCommand),
    /// Snapshot all tracked paths to a remote.
    Push {
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        remote: Option<String>,
        /// Snapshot message.
        #[arg(short, long)]
        message: Option<String>,
        /// Tag the snapshot (repeatable).
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Push to every remote in turn.
        #[arg(long)]
        all: bool,
    },
    /// List snapshots.
    Log { remote: String },
    /// List the files in a snapshot.
    Ls {
        remote: String,
        snapshot: String,
        path: Option<PathBuf>,
    },
    /// Show paths added, removed and changed between two snapshots.
    Diff {
        remote: String,
        a: String,
        b: String,
    },
    /// Restore a snapshot, or one sub-path of it, into a directory.
    #[command(alias = "pull")]
    Restore {
        remote: String,
        snapshot: String,
        path: Option<PathBuf>,
        /// Directory to restore into; created if missing.
        #[arg(long = "to", value_name = "DIR")]
        to: PathBuf,
        /// Restore into a non-empty directory, overwriting matching files.
        /// Never deletes files.
        #[arg(long)]
        force: bool,
    },
    /// Check the repository; --read-data also reads and hashes every pack.
    Verify {
        remote: String,
        /// Also read back and hash every pack file.
        #[arg(long)]
        read_data: bool,
    },
    /// Copy snapshots between remotes (all when none are named).
    Copy {
        from: String,
        to: String,
        snapshots: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RemoteCommand {
    /// Register a remote, creating a repository there if needed.
    Add {
        name: String,
        url: String,
        /// Refuse to write unless the path is on a mounted drive.
        #[arg(long)]
        require_mount: bool,
        /// Data pack size in MiB (default: 128 for cloud-synced folders, else engine default).
        #[arg(long, value_name = "MIB")]
        pack_size: Option<u32>,
        /// CA certificate for a self-signed rest-server.
        #[arg(long, value_name = "FILE")]
        cacert: Option<String>,
        /// File with `username:password` for a REST remote (mode 0600).
        #[arg(long, value_name = "FILE")]
        credentials_file: Option<String>,
        /// Create the repository without asking.
        #[arg(short, long)]
        yes: bool,
    },
    /// List remotes.
    List,
    /// Unregister a remote. Never deletes data at the remote.
    Remove { name: String },
}

/// Set up logging to stderr, with every line scrubbed of secrets.
pub fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => log::LevelFilter::Warn,
        1 => log::LevelFilter::Info,
        _ => log::LevelFilter::Debug,
    };
    let _ = env_logger::Builder::new()
        .filter_level(level)
        .format(|buf, record| {
            let line = bak_core::redact::scrub(&record.args().to_string());
            writeln!(buf, "[{}] {}", record.level(), line)
        })
        .try_init();
}

/// `BAK_CACHE_DIR` overrides the engine's default cache location.
pub fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("BAK_CACHE_DIR").map(PathBuf::from)
}

struct Ctx<'a> {
    cli: &'a Cli,
    engine: &'a dyn Engine,
    cwd: PathBuf,
    passphrase: OnceCell<Secret>,
}

pub fn run(cli: Cli, engine: &dyn Engine) -> Result<()> {
    let ctx = Ctx {
        cli: &cli,
        engine,
        cwd: std::env::current_dir().context("cannot read the current directory")?,
        passphrase: OnceCell::new(),
    };
    match &cli.command {
        Command::Init => cmd_init(&ctx),
        Command::Add { paths } => cmd_add(&ctx, paths),
        Command::Untrack { paths } => cmd_untrack(&ctx, paths),
        Command::Status => cmd_status(&ctx),
        Command::Remote(rc) => match rc {
            RemoteCommand::Add {
                name,
                url,
                require_mount,
                pack_size,
                cacert,
                credentials_file,
                yes,
            } => cmd_remote_add(
                &ctx,
                name,
                RemoteConfig {
                    url: url.clone(),
                    require_mount: *require_mount,
                    cacert: cacert.clone(),
                    pack_size_mib: *pack_size,
                    credentials_file: credentials_file.clone(),
                },
                *yes,
            ),
            RemoteCommand::List => cmd_remote_list(&ctx),
            RemoteCommand::Remove { name } => cmd_remote_remove(&ctx, name),
        },
        Command::Push {
            remote,
            message,
            tags,
            all,
        } => cmd_push(&ctx, remote.as_deref(), message.as_deref(), tags, *all),
        Command::Log { remote } => cmd_log(&ctx, remote),
        Command::Ls {
            remote,
            snapshot,
            path,
        } => cmd_ls(&ctx, remote, snapshot, path.as_deref()),
        Command::Diff { remote, a, b } => cmd_diff(&ctx, remote, a, b),
        Command::Restore {
            remote,
            snapshot,
            path,
            to,
            force,
        } => cmd_restore(&ctx, remote, snapshot, path.as_deref(), to, *force),
        Command::Verify { remote, read_data } => cmd_verify(&ctx, remote, *read_data),
        Command::Copy {
            from,
            to,
            snapshots,
        } => cmd_copy(&ctx, from, to, snapshots),
    }
}

impl Ctx<'_> {
    fn project(&self) -> Result<Project> {
        Ok(Project::discover(&self.cwd)?)
    }

    fn remote(&self, project: &Project, name: &str) -> Result<RemoteSpec> {
        Ok(RemoteSpec::from_config(name, project.remote(name)?)?)
    }

    /// The repository passphrase: `--password-file`, then `BAK_PASSWORD`, then a prompt.
    fn passphrase(&self, confirm: bool) -> Result<&Secret> {
        if let Some(p) = self.passphrase.get() {
            return Ok(p);
        }
        let secret = if let Some(file) = &self.cli.password_file {
            check_private_file(file)?;
            let text = fs::read_to_string(file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            Secret::new(text.lines().next().unwrap_or("").to_string())
        } else if let Ok(p) = std::env::var("BAK_PASSWORD") {
            Secret::new(p)
        } else if std::io::stdin().is_terminal() {
            let p = Secret::new(rpassword::prompt_password("Repository passphrase: ")?);
            if confirm {
                let again = Secret::new(rpassword::prompt_password("Repeat passphrase: ")?);
                if again != p {
                    bail!("passphrases do not match");
                }
            }
            p
        } else {
            bail!("no passphrase: set BAK_PASSWORD, pass --password-file, or run in a terminal");
        };
        if secret.expose().is_empty() {
            bail!("the passphrase is empty");
        }
        Ok(self.passphrase.get_or_init(|| secret))
    }

    fn print_json(&self, value: &impl serde::Serialize) -> Result<()> {
        println!("{}", serde_json::to_string_pretty(value)?);
        Ok(())
    }
}

fn cmd_init(ctx: &Ctx) -> Result<()> {
    let project = Project::init(&ctx.cwd)?;
    println!(
        "initialised bak project `{}` in {}",
        project.config.project.name,
        project.root.display()
    );
    println!("tracking: . (the whole project). Exclude files with {IGNORE_FILE}.");
    Ok(())
}

fn cmd_add(ctx: &Ctx, paths: &[PathBuf]) -> Result<()> {
    let mut project = ctx.project()?;
    let rel = paths
        .iter()
        .map(|p| project.relative_path(&ctx.cwd, p, true))
        .collect::<Result<Vec<_>, _>>()?;
    let added = project.track(rel);
    project.save()?;
    for p in &added {
        println!("tracking {p}");
    }
    if added.is_empty() {
        println!("already tracked");
    }
    Ok(())
}

fn cmd_untrack(ctx: &Ctx, paths: &[PathBuf]) -> Result<()> {
    let mut project = ctx.project()?;
    let rel = paths
        .iter()
        .map(|p| project.relative_path(&ctx.cwd, p, false))
        .collect::<Result<Vec<_>, _>>()?;
    let removed = project.untrack(&rel);
    project.save()?;
    for p in &removed {
        println!("untracked {p}");
    }
    if removed.len() < rel.len() {
        println!("some paths were not tracked");
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct RemoteStatus {
    name: String,
    url: String,
    latest: Option<SnapshotInfo>,
    snapshots: Option<usize>,
    error: Option<String>,
}

fn cmd_status(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    let mut remotes = Vec::new();
    for name in project.config.remotes.keys() {
        let spec = ctx.remote(&project, name)?;
        let result = ctx
            .passphrase(false)
            .and_then(|p| Ok(ctx.engine.snapshots(&spec, p)?));
        let (latest, count, error) = match result {
            Ok(snaps) => (snaps.last().cloned(), Some(snaps.len()), None),
            Err(e) => (None, None, Some(bak_core::redact::scrub(&format!("{e:#}")))),
        };
        remotes.push(RemoteStatus {
            name: name.clone(),
            url: spec.display_url(),
            latest,
            snapshots: count,
            error,
        });
    }
    if ctx.cli.json {
        return ctx.print_json(&serde_json::json!({
            "project": project.config.project.name,
            "root": project.root,
            "tracked": project.config.project.tracked,
            "remotes": remotes,
        }));
    }
    println!(
        "project {}  ({})",
        project.config.project.name,
        project.root.display()
    );
    println!("tracked:");
    for t in &project.config.project.tracked {
        println!("  {t}");
    }
    if remotes.is_empty() {
        println!("no remotes (add one with `bak remote add <name> <url>`)");
    }
    for r in &remotes {
        let state = match (&r.latest, &r.error) {
            (_, Some(e)) => format!("unreachable: {e}"),
            (Some(s), None) => format!(
                "latest {} at {} ({} snapshots)",
                s.short_id,
                s.time,
                r.snapshots.unwrap_or(0)
            ),
            (None, None) => "no snapshots yet".to_string(),
        };
        println!("{:<10} {}\n           {state}", r.name, r.url);
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("{question} Re-run with --yes to confirm.");
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

fn cmd_remote_add(ctx: &Ctx, name: &str, mut cfg: RemoteConfig, yes: bool) -> Result<()> {
    let mut project = ctx.project()?;
    if project.config.remotes.contains_key(name) {
        return Err(bak_core::Error::RemoteExists(name.to_string()).into());
    }
    if let Location::Local(path) = Location::parse(&cfg.url)? {
        if cfg.pack_size_mib.is_none() && is_cloud_synced_folder(&path) {
            cfg.pack_size_mib = Some(CLOUD_FOLDER_PACK_SIZE_MIB);
            println!(
                "{} is a cloud-synced folder: using {CLOUD_FOLDER_PACK_SIZE_MIB} MiB packs",
                path.display()
            );
        }
    }
    let spec = RemoteSpec::from_config(name, &cfg)?;
    // Before anything is created: an unplugged drive must not become a repository on the system disk.
    spec.check_mount()?;

    if ctx.engine.exists(&spec)? {
        let pass = ctx.passphrase(false)?;
        let n = ctx
            .engine
            .snapshots(&spec, pass)
            .context("a repository exists there but cannot be opened with this passphrase")?
            .len();
        println!(
            "found existing repository at {} ({n} snapshots)",
            spec.display_url()
        );
    } else {
        if !yes
            && !confirm(&format!(
                "No repository at {}. Create one?",
                spec.display_url()
            ))?
        {
            bail!("not created");
        }
        let pass = ctx.passphrase(true)?;
        let chunker_from = chunker_template(ctx, &project, pass);
        ctx.engine.init(
            &spec,
            pass,
            &InitOptions {
                pack_size_mib: cfg.pack_size_mib,
                chunker_from,
            },
        )?;
        println!("created repository at {}", spec.display_url());
    }
    project.config.remotes.insert(name.to_string(), cfg);
    project.save()?;
    println!("added remote {name}");
    Ok(())
}

/// The first existing remote whose repository opens, so a new repository can
/// share its chunker parameters and `copy` between them deduplicates.
fn chunker_template(ctx: &Ctx, project: &Project, pass: &Secret) -> Option<RemoteSpec> {
    project.config.remotes.keys().find_map(|n| {
        let spec = ctx.remote(project, n).ok()?;
        if spec.check_mount().is_err() {
            return None;
        }
        ctx.engine.snapshots(&spec, pass).ok()?;
        Some(spec)
    })
}

fn cmd_remote_list(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    for (name, cfg) in &project.config.remotes {
        let flags = if cfg.require_mount {
            "  (require_mount)"
        } else {
            ""
        };
        println!("{name:<10} {}{flags}", cfg.url);
    }
    Ok(())
}

fn cmd_remote_remove(ctx: &Ctx, name: &str) -> Result<()> {
    let mut project = ctx.project()?;
    project.remote(name)?;
    project.config.remotes.remove(name);
    project.save()?;
    println!("removed remote {name} (no data at the remote was touched)");
    Ok(())
}

fn cmd_push(
    ctx: &Ctx,
    remote: Option<&str>,
    message: Option<&str>,
    tags: &[String],
    all: bool,
) -> Result<()> {
    let project = ctx.project()?;
    if project.config.project.tracked.is_empty() {
        bail!("nothing is tracked (use `bak add <path>`)");
    }
    let names: Vec<String> = if all {
        project.config.remotes.keys().cloned().collect()
    } else {
        vec![remote.unwrap_or_default().to_string()]
    };
    if names.is_empty() {
        bail!("no remotes configured");
    }
    let mut failures = Vec::new();
    for name in &names {
        if let Err(e) = push_one(ctx, &project, name, message, tags) {
            if !all {
                return Err(e);
            }
            eprintln!(
                "bak: push to {name} failed: {}",
                bak_core::redact::scrub(&format!("{e:#}"))
            );
            failures.push(name.clone());
        }
    }
    if !failures.is_empty() {
        bail!("push failed for: {}", failures.join(", "));
    }
    Ok(())
}

fn push_one(
    ctx: &Ctx,
    project: &Project,
    name: &str,
    message: Option<&str>,
    tags: &[String],
) -> Result<()> {
    let spec = ctx.remote(project, name)?;
    spec.check_mount()?;
    if !ctx.engine.exists(&spec)? {
        bail!(
            "no repository at {} (re-add the remote to create one)",
            spec.display_url()
        );
    }
    let pass = ctx.passphrase(false)?;
    let s = ctx.engine.snapshot(
        &spec,
        pass,
        &SnapshotRequest {
            root: &project.root,
            paths: &project.config.project.tracked,
            host: &project.config.project.name,
            message,
            tags,
            ignore_file: IGNORE_FILE,
        },
    )?;
    log::info!("data blobs added: {}", s.data_blobs_added);
    if ctx.cli.json {
        return ctx.print_json(&s);
    }
    println!(
        "pushed {} to {name}: {} files ({} new, {} changed), {} added, {} stored, {:.1}s",
        s.snapshot.short_id,
        s.snapshot.files,
        s.files_new,
        s.files_changed,
        human(s.bytes_added),
        human(s.bytes_added_stored),
        s.seconds
    );
    Ok(())
}

fn cmd_log(ctx: &Ctx, remote: &str) -> Result<()> {
    let project = ctx.project()?;
    let spec = ctx.remote(&project, remote)?;
    let snaps = ctx.engine.snapshots(&spec, ctx.passphrase(false)?)?;
    if ctx.cli.json {
        return ctx.print_json(&snaps);
    }
    println!(
        "{:<8}  {:<25}  {:>9}  {:>7}  {:<12}  MESSAGE",
        "ID", "TIME", "SIZE", "FILES", "TAGS"
    );
    for s in &snaps {
        println!(
            "{:<8}  {:<25}  {:>9}  {:>7}  {:<12}  {}",
            s.short_id,
            s.time,
            human(s.size),
            s.files,
            s.tags.join(","),
            s.message.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

fn cmd_ls(ctx: &Ctx, remote: &str, snapshot: &str, path: Option<&Path>) -> Result<()> {
    let project = ctx.project()?;
    let spec = ctx.remote(&project, remote)?;
    let entries = ctx.engine.entries(
        &spec,
        ctx.passphrase(false)?,
        &SnapshotRef::parse(snapshot),
        path,
    )?;
    if ctx.cli.json {
        return ctx.print_json(&entries);
    }
    for e in &entries {
        let kind = match e.kind {
            EntryKind::Dir => 'd',
            EntryKind::Symlink => 'l',
            EntryKind::File => '-',
            EntryKind::Other => '?',
        };
        let mode = e
            .mode
            .map(|m| format!("{:o}", m & 0o7777))
            .unwrap_or_default();
        let link = e
            .link_target
            .as_deref()
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        println!("{kind} {mode:>4} {:>9}  {}{link}", human(e.size), e.path);
    }
    Ok(())
}

fn cmd_diff(ctx: &Ctx, remote: &str, a: &str, b: &str) -> Result<()> {
    let project = ctx.project()?;
    let spec = ctx.remote(&project, remote)?;
    let pass = ctx.passphrase(false)?;
    let ea = ctx
        .engine
        .entries(&spec, pass, &SnapshotRef::parse(a), None)?;
    let eb = ctx
        .engine
        .entries(&spec, pass, &SnapshotRef::parse(b), None)?;
    let report = diff::diff(&ea, &eb);
    if ctx.cli.json {
        return ctx.print_json(&report);
    }
    for p in &report.added {
        println!("+ {p}");
    }
    for p in &report.removed {
        println!("- {p}");
    }
    for p in &report.changed {
        println!("M {p}");
    }
    if report.is_empty() {
        println!("no differences");
    }
    Ok(())
}

fn cmd_restore(
    ctx: &Ctx,
    remote: &str,
    snapshot: &str,
    path: Option<&Path>,
    to: &Path,
    force: bool,
) -> Result<()> {
    let project = ctx.project()?;
    let spec = ctx.remote(&project, remote)?;
    let dest = ctx.cwd.join(to);
    if dest.exists() {
        let non_empty = fs::read_dir(&dest)
            .with_context(|| format!("cannot read {}", dest.display()))?
            .next()
            .is_some();
        if non_empty && !force {
            bail!(
                "{} is not empty; use --force to overwrite matching files (nothing is deleted)",
                dest.display()
            );
        }
    } else {
        fs::create_dir_all(&dest).with_context(|| format!("cannot create {}", dest.display()))?;
    }
    let s = ctx.engine.restore(
        &spec,
        ctx.passphrase(false)?,
        &SnapshotRef::parse(snapshot),
        path,
        &dest,
    )?;
    println!(
        "restored {} files, {} dirs, {} into {}",
        s.files,
        s.dirs,
        human(s.bytes),
        dest.display()
    );
    Ok(())
}

fn cmd_verify(ctx: &Ctx, remote: &str, read_data: bool) -> Result<()> {
    let project = ctx.project()?;
    let spec = ctx.remote(&project, remote)?;
    let report = ctx
        .engine
        .verify(&spec, ctx.passphrase(false)?, read_data)?;
    if ctx.cli.json {
        ctx.print_json(&report)?;
    }
    let errors = report.problems.iter().filter(|p| p.error).count();
    for p in &report.problems {
        let level = if p.error { "error" } else { "warning" };
        eprintln!("{level}: {}", p.message);
    }
    if errors > 0 {
        bail!("verify found {errors} error(s) in {}", spec.display_url());
    }
    println!(
        "{} ok{}{}",
        spec.display_url(),
        if read_data { " (all data read)" } else { "" },
        if report.problems.is_empty() {
            String::new()
        } else {
            format!(", {} warning(s)", report.problems.len())
        }
    );
    Ok(())
}

fn cmd_copy(ctx: &Ctx, from: &str, to: &str, snapshots: &[String]) -> Result<()> {
    let project = ctx.project()?;
    let src = ctx.remote(&project, from)?;
    let dst = ctx.remote(&project, to)?;
    dst.check_mount()?;
    if !ctx.engine.exists(&dst)? {
        return Err(anyhow!("no repository at {}", dst.display_url()));
    }
    let refs: Vec<SnapshotRef> = snapshots.iter().map(|s| SnapshotRef::parse(s)).collect();
    let s = ctx.engine.copy(&src, &dst, ctx.passphrase(false)?, &refs)?;
    println!(
        "copied {} snapshot(s) from {from} to {to}; {} already present",
        s.copied, s.already_present
    );
    Ok(())
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}
