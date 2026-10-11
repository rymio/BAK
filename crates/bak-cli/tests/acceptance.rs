//! The twelve acceptance tests from the SOW. Every test works in its own
//! temporary directory; temporary directories stand in for disk and USB remotes.
//!
//! `BAK_TEST_BIG_MB` overrides the size of the large random file (default 200).

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

const PASSPHRASE: &str = "correct horse battery staple 7f3a";
const MIB: usize = 1024 * 1024;

fn big_file_mib() -> usize {
    std::env::var("BAK_TEST_BIG_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
}

/// A project directory, a place for remotes, and an isolated engine cache.
struct Env {
    _tmp: TempDir,
    root: PathBuf,
    proj: PathBuf,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("bak-test-")
            .tempdir()
            .unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let proj = root.join("proj");
        fs::create_dir_all(&proj).unwrap();
        Self {
            _tmp: tmp,
            root,
            proj,
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("bak").unwrap();
        c.current_dir(&self.proj)
            .env("BAK_PASSWORD", PASSPHRASE)
            .env("BAK_CACHE_DIR", self.root.join("cache"))
            .env_remove("BAK_REST_USER")
            .env_remove("BAK_REST_PASSWORD")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().unwrap()
    }

    /// Run and require success; returns stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "bak {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let mut a = args.to_vec();
        a.push("--json");
        serde_json::from_str(&self.ok(&a)).unwrap()
    }

    /// `bak init` plus a local remote called `disk`.
    fn init_with_disk(&self) -> PathBuf {
        let repo = self.path("remotes/disk");
        self.ok(&["init"]);
        self.ok(&["remote", "add", "disk", repo.to_str().unwrap(), "--yes"]);
        repo
    }

    fn push(&self, remote: &str, message: &str) -> serde_json::Value {
        self.json(&["push", remote, "-m", message])
    }
}

/// Deterministic pseudo-random bytes (xorshift64*), so test data is the same on every run.
struct Prng(u64);

impl Prng {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let v = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

fn write_random(path: &Path, size: usize, seed: u64) {
    let mut rng = Prng(seed | 1);
    let mut f = fs::File::create(path).unwrap();
    let mut buf = vec![0u8; MIB];
    let mut left = size;
    while left > 0 {
        let n = left.min(MIB);
        rng.fill(&mut buf[..n]);
        f.write_all(&buf[..n]).unwrap();
        left -= n;
    }
}

/// Fixture from test 1. `small_files` controls the `.git/objects`-shaped directory.
fn make_fixture(proj: &Path, big_mib: usize, small_files: usize) {
    fs::create_dir_all(proj.join("src/deep/er/still")).unwrap();
    fs::write(proj.join("src/deep/er/still/leaf.txt"), "leaf\n").unwrap();
    fs::write(proj.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(proj.join("empty"), "").unwrap();
    symlink("src/main.rs", proj.join("link-to-main")).unwrap();
    symlink("../does-not-exist", proj.join("dangling")).unwrap();
    let exe = proj.join("run.sh");
    fs::write(&exe, "#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    make_non_utf8_file(proj);
    if big_mib > 0 {
        write_random(&proj.join("big.bin"), big_mib * MIB, 42);
    }
    let mut rng = Prng(7);
    for i in 0..small_files {
        let dir = proj.join(format!(".git/objects/{:02x}", i % 256));
        if i < 256 {
            fs::create_dir_all(&dir).unwrap();
        }
        let mut buf = vec![0u8; 64 + (i % 512)];
        rng.fill(&mut buf);
        fs::write(dir.join(format!("{i:038x}")), buf).unwrap();
    }
}

/// A file whose name is not valid UTF-8. APFS (macOS) refuses such names, so
/// on that filesystem the case is skipped with a notice.
fn make_non_utf8_file(proj: &Path) {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let name = OsStr::from_bytes(b"latin1-caf\xe9.txt");
    match fs::write(proj.join(name), "non-utf8 name\n") {
        Ok(()) => {}
        Err(e) => eprintln!("NOTICE: non-UTF-8 file name not supported here ({e}); case skipped"),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Item {
    Dir(u32),
    File(u32, [u8; 32]),
    Link(PathBuf),
}

/// Snapshot of a directory tree: relative path -> kind, mode, content digest or link target.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Item> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Item>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            let meta = fs::symlink_metadata(&p).unwrap();
            let mode = meta.permissions().mode() & 0o7777;
            if meta.file_type().is_symlink() {
                out.insert(rel, Item::Link(fs::read_link(&p).unwrap()));
            } else if meta.is_dir() {
                out.insert(rel, Item::Dir(mode));
                walk(base, &p, out);
            } else {
                out.insert(rel, Item::File(mode, digest(&p)));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Content digest: two independent 128-bit FNV-style lanes. Plenty for equality checks.
fn digest(path: &Path) -> [u8; 32] {
    let mut f = fs::File::open(path).unwrap();
    let mut buf = vec![0u8; MIB];
    let (mut a, mut b) = (0xcbf2_9ce4_8422_2325_u128, 0x6c62_272e_07bb_0142_u128);
    loop {
        let n = f.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        for &x in &buf[..n] {
            a = (a ^ u128::from(x)).wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
            b = (b ^ u128::from(x)).wrapping_mul(0x0000_0100_0000_0000_0000_0000_0000_01b3);
        }
    }
    let mut d = [0u8; 32];
    d[..16].copy_from_slice(&a.to_le_bytes());
    d[16..].copy_from_slice(&b.to_le_bytes());
    d
}

fn assert_same_tree(a: &Path, b: &Path) {
    assert_same_tree_except(a, b, &[]);
}

fn assert_same_tree_except(a: &Path, b: &Path, skip: &[&str]) {
    let (mut ta, mut tb) = (tree(a), tree(b));
    for s in skip {
        ta.remove(Path::new(s));
        tb.remove(Path::new(s));
    }
    let only_a: Vec<_> = ta.keys().filter(|k| !tb.contains_key(*k)).collect();
    let only_b: Vec<_> = tb.keys().filter(|k| !ta.contains_key(*k)).collect();
    assert!(
        only_a.is_empty() && only_b.is_empty(),
        "missing {only_a:?}, extra {only_b:?}"
    );
    for (k, v) in &ta {
        assert_eq!(v, &tb[k], "mismatch at {}", k.display());
    }
}

fn data_files(repo: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in fs::read_dir(repo.join("data")).unwrap() {
        for f in fs::read_dir(sub.unwrap().path()).unwrap() {
            out.push(f.unwrap().path());
        }
    }
    out.sort();
    out
}

fn count_files(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            if p.is_dir() {
                count_files(&p)
            } else {
                1
            }
        })
        .sum()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        let meta = fs::symlink_metadata(&p).unwrap();
        if meta.file_type().is_symlink() {
            symlink(fs::read_link(&p).unwrap(), &dest).unwrap();
        } else if meta.is_dir() {
            copy_dir(&p, &dest);
        } else {
            fs::copy(&p, &dest).unwrap();
        }
    }
}

fn on_path(program: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// 1. Round trip.
#[test]
fn t01_round_trip() {
    let env = Env::new();
    make_fixture(&env.proj, big_file_mib(), 20_000);
    env.init_with_disk();
    env.push("disk", "round trip");
    let out = env.path("restored");
    env.ok(&["restore", "disk", "latest", "--to", out.to_str().unwrap()]);
    assert_same_tree(&env.proj, &out);
}

// 2. Incremental: a second push with no changes adds no data blobs.
#[test]
fn t02_incremental() {
    let env = Env::new();
    make_fixture(&env.proj, 8, 500);
    env.init_with_disk();
    let first = env.push("disk", "first");
    assert!(first["data_blobs_added"].as_u64().unwrap() > 0);
    let second = env.push("disk", "second");
    assert_eq!(second["data_blobs_added"], 0, "second push: {second}");
    assert_eq!(second["files_changed"], 0);
}

// 3. Dedup: a 1 MiB overwrite in the middle of the big file adds little data.
#[test]
fn t03_dedup() {
    let env = Env::new();
    let big = big_file_mib();
    make_fixture(&env.proj, big, 0);
    env.init_with_disk();
    env.push("disk", "before");
    let mut f = fs::OpenOptions::new()
        .write(true)
        .open(env.proj.join("big.bin"))
        .unwrap();
    f.seek(SeekFrom::Start((big * MIB / 2) as u64)).unwrap();
    let mut patch = vec![0u8; MIB];
    Prng(99).fill(&mut patch);
    f.write_all(&patch).unwrap();
    drop(f);
    let after = env.push("disk", "after");
    let added = after["bytes_added"].as_u64().unwrap();
    eprintln!("dedup: 1 MiB overwrite added {added} bytes");
    assert!(added < 20 * MIB as u64, "added {added} bytes");
}

// 4. History: three snapshots, and the first still restores to the original content.
#[test]
fn t04_history() {
    let env = Env::new();
    make_fixture(&env.proj, 1, 100);
    env.init_with_disk();
    let original = env.path("original");
    copy_dir(&env.proj, &original);
    env.push("disk", "one");
    fs::write(env.proj.join("src/main.rs"), "fn main() { two() }\n").unwrap();
    env.push("disk", "two");
    fs::remove_file(env.proj.join("empty")).unwrap();
    fs::write(env.proj.join("new.txt"), "three\n").unwrap();
    env.push("disk", "three");

    let log = env.json(&["log", "disk"]);
    let snaps = log.as_array().unwrap();
    assert_eq!(snaps.len(), 3);
    assert_eq!(snaps[0]["message"], "one");
    let first = snaps[0]["short_id"].as_str().unwrap();

    let out = env.path("restored-first");
    env.ok(&["restore", "disk", first, "--to", out.to_str().unwrap()]);
    assert_eq!(
        fs::read_to_string(out.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert!(out.join("empty").exists());
    assert!(!out.join("new.txt").exists());
    for (path, item) in tree(&original) {
        if let Item::File(_, d) = item {
            assert_eq!(digest(&out.join(&path)), d, "{}", path.display());
        }
    }

    let diff = env.json(&["diff", "disk", first, "latest"]);
    assert_eq!(diff["added"], serde_json::json!(["new.txt"]));
    assert!(diff["removed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "empty"));
    assert!(diff["changed"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "src/main.rs"));
}

// 5. Sub-path restore writes only that directory.
#[test]
fn t05_sub_path_restore() {
    let env = Env::new();
    make_fixture(&env.proj, 1, 50);
    env.init_with_disk();
    env.push("disk", "snap");
    let out = env.path("partial");
    env.ok(&[
        "restore",
        "disk",
        "latest",
        "src/deep",
        "--to",
        out.to_str().unwrap(),
    ]);
    let top: Vec<_> = fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(top, vec!["src"]);
    let src: Vec<_> = fs::read_dir(out.join("src"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(src, vec!["deep"]);
    assert_same_tree(&env.proj.join("src/deep"), &out.join("src/deep"));

    // A non-empty target is refused without --force, and --force deletes nothing.
    fs::write(out.join("keep.txt"), "mine").unwrap();
    assert!(!env
        .run(&[
            "restore",
            "disk",
            "latest",
            "src/deep",
            "--to",
            out.to_str().unwrap()
        ])
        .status
        .success());
    env.ok(&[
        "restore",
        "disk",
        "latest",
        "src/deep",
        "--to",
        out.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(fs::read_to_string(out.join("keep.txt")).unwrap(), "mine");
}

// 6. Corruption: a flipped byte in a pack is found by verify --read-data.
#[test]
fn t06_corruption() {
    let env = Env::new();
    make_fixture(&env.proj, 4, 0);
    let repo = env.init_with_disk();
    env.push("disk", "snap");
    env.ok(&["verify", "disk", "--read-data"]);

    // Work on a copy, never on the original repository.
    let copy = env.path("remotes/corrupt");
    copy_dir(&repo, &copy);
    env.ok(&["remote", "add", "corrupt", copy.to_str().unwrap(), "--yes"]);
    let target = data_files(&copy)
        .into_iter()
        .max_by_key(|p| fs::metadata(p).unwrap().len())
        .unwrap();
    let pack_id = target.file_name().unwrap().to_str().unwrap().to_string();
    let mut bytes = fs::read(&target).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x01;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&target, bytes).unwrap();

    let out = env.run(&["verify", "corrupt", "--read-data"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "verify passed on a corrupt pack: {stderr}"
    );
    assert!(
        stderr.contains(&pack_id) || stderr.contains(&pack_id[..8]),
        "damaged pack {pack_id} not named in: {stderr}"
    );
}

// 7. Interrupted push: the next push succeeds, the repo verifies, old snapshots restore.
#[test]
fn t07_interrupted_push() {
    let env = Env::new();
    make_fixture(&env.proj, 0, 100);
    env.init_with_disk();
    env.push("disk", "before");
    write_random(&env.proj.join("big.bin"), big_file_mib().max(200) * MIB, 5);

    let mut child = env
        .cmd()
        .args(["push", "disk", "-m", "interrupted"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut killed = false;
    for line in BufReader::new(stderr).lines() {
        let line = line.unwrap();
        if line.starts_with("progress: backing up") && progress_pct(&line) >= 50 {
            child.kill().unwrap();
            killed = true;
            break;
        }
    }
    let status = child.wait().unwrap();
    assert!(
        killed && !status.success(),
        "push finished before it could be interrupted"
    );

    let log = env.json(&["log", "disk"]);
    assert_eq!(
        log.as_array().unwrap().len(),
        1,
        "interrupted push left a snapshot"
    );
    env.push("disk", "after");
    env.ok(&["verify", "disk", "--read-data"]);
    let out = env.path("restored-before");
    env.ok(&[
        "restore",
        "disk",
        log[0]["short_id"].as_str().unwrap(),
        "--to",
        out.to_str().unwrap(),
    ]);
    assert!(!out.join("big.bin").exists());
    assert_eq!(
        fs::read_to_string(out.join("src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
}

fn progress_pct(line: &str) -> u32 {
    line.trim_end_matches('%')
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

// 8. Restic interop.
#[test]
fn t08_restic_interop() {
    if !on_path("restic") {
        eprintln!("NOTICE: restic not on PATH; restic interop test skipped");
        return;
    }
    let env = Env::new();
    make_fixture(&env.proj, 4, 200);
    let repo = env.init_with_disk();
    env.push("disk", "for restic");
    let restic = |args: &[&str]| {
        Command::new("restic")
            .args(["-r", repo.to_str().unwrap(), "--no-cache"])
            .args(args)
            .env("RESTIC_PASSWORD", PASSPHRASE)
            .output()
            .unwrap()
    };
    let check = restic(&["check", "--read-data"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let out = env.path("restic-restore");
    let restore = restic(&["restore", "latest", "--target", out.to_str().unwrap()]);
    assert!(
        restore.status.success(),
        "{}",
        String::from_utf8_lossy(&restore.stderr)
    );
    assert_same_tree(&env.proj, &out);
}

// 9. REST remote: tests 1 and 2 against a local rest-server.
#[test]
fn t09_rest_remote() {
    if !on_path("rest-server") {
        eprintln!("NOTICE: rest-server not on PATH; REST remote test skipped");
        return;
    }
    let env = Env::new();
    let port = 18000 + (std::process::id() % 1000) as u16;
    let mut server = Command::new("rest-server")
        .args(["--path", env.path("rest-data").to_str().unwrap()])
        .args(["--listen", &format!("127.0.0.1:{port}"), "--no-auth"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let result = std::panic::catch_unwind(|| {
        make_fixture(&env.proj, 8, 500);
        env.ok(&["init"]);
        let url = format!("rest:http://127.0.0.1:{port}/proj");
        env.ok(&["remote", "add", "home", &url, "--yes"]);
        env.push("home", "first");
        let second = env.push("home", "second");
        assert_eq!(second["data_blobs_added"], 0);
        let out = env.path("rest-restore");
        env.ok(&["restore", "home", "latest", "--to", out.to_str().unwrap()]);
        assert_same_tree(&env.proj, &out);
    });
    let _ = server.kill();
    let _ = server.wait();
    result.unwrap();
}

// 10. Mount guard: nothing is written to a require_mount remote on the system disk.
#[test]
fn t10_mount_guard() {
    let env = Env::new();
    make_fixture(&env.proj, 0, 10);
    env.ok(&["init"]);

    // remote add --require-mount refuses before creating anything.
    let usb = env.path("media/USB1/bak/proj");
    let out = env.run(&[
        "remote",
        "add",
        "usb",
        usb.to_str().unwrap(),
        "--require-mount",
        "--yes",
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("mount guard"));
    assert!(
        !env.path("media").exists(),
        "remote add wrote to an unmounted path"
    );
    assert!(!fs::read_to_string(env.proj.join(".bak/config.toml"))
        .unwrap()
        .contains("usb"));

    // push refuses when an existing remote is marked require_mount.
    let repo = env.path("remotes/disk");
    env.ok(&["remote", "add", "disk", repo.to_str().unwrap(), "--yes"]);
    let before = count_files(&repo);
    let cfg_path = env.proj.join(".bak/config.toml");
    let cfg = fs::read_to_string(&cfg_path).unwrap();
    let cfg = cfg.replace(
        &format!("url = \"{}\"", repo.display()),
        &format!("url = \"{}\"\nrequire_mount = true", repo.display()),
    );
    fs::write(&cfg_path, cfg).unwrap();
    let out = env.run(&["push", "disk"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("mount guard"));
    assert_eq!(count_files(&repo), before, "push wrote to a guarded remote");
}

// 11. Secrets never appear in output or config, including on the error path.
#[test]
fn t11_secrets() {
    let env = Env::new();
    make_fixture(&env.proj, 0, 10);
    env.init_with_disk();
    let mut all = String::new();
    for args in [
        vec!["-vv", "push", "disk", "-m", "verbose"],
        vec!["-vv", "log", "disk"],
        vec!["-vv", "verify", "disk"],
        vec!["-vv", "status"],
    ] {
        let out = env.run(&args);
        assert!(out.status.success());
        all += &String::from_utf8_lossy(&out.stdout);
        all += &String::from_utf8_lossy(&out.stderr);
    }
    assert!(
        all.contains("[DEBUG]") || all.contains("[INFO]"),
        "-vv produced no logs"
    );

    // A REST remote that fails: wrong host, credentials from the environment.
    let user = "bakuser-6d2e";
    let rest_pw = "rest-pw-91c4!x";
    let cfg_path = env.proj.join(".bak/config.toml");
    let mut cfg = fs::read_to_string(&cfg_path).unwrap();
    cfg += "\n[remotes.home]\nurl = \"rest:http://127.0.0.1:9/proj\"\n";
    fs::write(&cfg_path, &cfg).unwrap();
    for args in [vec!["-vv", "push", "home"], vec!["-vv", "status"]] {
        let out = env
            .cmd()
            .args(&args)
            .env("BAK_REST_USER", user)
            .env("BAK_REST_PASSWORD", rest_pw)
            .output()
            .unwrap();
        all += &String::from_utf8_lossy(&out.stdout);
        all += &String::from_utf8_lossy(&out.stderr);
    }
    // A malformed URL makes the REST backend echo the URL in its error.
    cfg += "\n[remotes.bad]\nurl = \"rest:http://[::1/proj\"\n";
    fs::write(&cfg_path, &cfg).unwrap();
    let out = env
        .cmd()
        .args(["-vv", "push", "bad"])
        .env("BAK_REST_USER", user)
        .env("BAK_REST_PASSWORD", rest_pw)
        .output()
        .unwrap();
    assert!(!out.status.success());
    all += &String::from_utf8_lossy(&out.stderr);

    let cfg = fs::read_to_string(&cfg_path).unwrap();
    for secret in [PASSPHRASE, rest_pw, "rest-pw-91c4%21x", user] {
        assert!(!all.contains(secret), "secret {secret:?} leaked:\n{all}");
        assert!(!cfg.contains(secret), "secret {secret:?} in config");
    }
}

// 12. Hygiene: no `unsafe` in the bak crates. fmt and clippy run in scripts/check.sh.
#[test]
fn t12_no_unsafe() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut hits = Vec::new();
    for krate in ["bak-core", "bak-engine-rustic", "bak-cli"] {
        let src = crates.join(krate).join("src");
        for e in fs::read_dir(&src).unwrap() {
            let p = e.unwrap().path();
            let text = fs::read_to_string(&p).unwrap();
            if text
                .lines()
                .any(|l| l.contains("unsafe ") && !l.trim_start().starts_with("//"))
            {
                hits.push(p);
            }
        }
    }
    assert!(hits.is_empty(), "unsafe found in {hits:?}");
    let ws = fs::read_to_string(crates.parent().unwrap().join("Cargo.toml")).unwrap();
    assert!(ws.contains("unsafe_code = \"forbid\""));
}

// Extra: copy between remotes deduplicates when chunker parameters are shared.
#[test]
fn t13_copy() {
    let env = Env::new();
    make_fixture(&env.proj, 4, 100);
    env.init_with_disk();
    env.push("disk", "one");
    fs::write(env.proj.join("src/main.rs"), "fn main() { 2 }\n").unwrap();
    env.push("disk", "two");
    let usb = env.path("remotes/usb");
    env.ok(&["remote", "add", "usb", usb.to_str().unwrap(), "--yes"]);
    let out = env.ok(&["copy", "disk", "usb"]);
    assert!(out.contains("copied 2 snapshot"), "{out}");
    let out = env.ok(&["copy", "disk", "usb"]);
    assert!(out.contains("copied 0 snapshot"), "{out}");
    assert_eq!(env.json(&["log", "usb"]).as_array().unwrap().len(), 2);
    env.ok(&["verify", "usb", "--read-data"]);
    let restored = env.path("from-usb");
    env.ok(&[
        "restore",
        "usb",
        "latest",
        "--to",
        restored.to_str().unwrap(),
    ]);
    // Adding the usb remote changed config.toml after the snapshot was taken.
    assert_same_tree_except(&env.proj, &restored, &[".bak/config.toml"]);
}
