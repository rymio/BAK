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
    /// Extra environment for every `bak` run (REST credentials).
    vars: std::cell::RefCell<Vec<(String, String)>>,
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
            vars: Default::default(),
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
        for (k, v) in self.vars.borrow().iter() {
            c.env(k, v);
        }
        c
    }

    fn set_var(&self, key: &str, value: &str) {
        let mut vars = self.vars.borrow_mut();
        vars.retain(|(k, _)| k != key);
        vars.push((key.to_string(), value.to_string()));
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
    }));
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
    for krate in ["bak-core", "bak-engine-rustic", "bak-cli", "bak-server"] {
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

// Extra: size reports stored bytes that match the repository on disk.
#[test]
fn t14_size() {
    let env = Env::new();
    make_fixture(&env.proj, 4, 50);
    let repo = env.init_with_disk();
    env.push("disk", "one");
    env.push("disk", "two");
    let v = env.json(&["size", "disk"]);
    let s = &v[0]["stats"];
    let on_disk: u64 = {
        fn walk(p: &Path) -> u64 {
            fs::read_dir(p)
                .unwrap()
                .map(|e| {
                    let p = e.unwrap().path();
                    if p.is_dir() {
                        walk(&p)
                    } else {
                        fs::metadata(&p).unwrap().len()
                    }
                })
                .sum()
        }
        walk(&repo)
    };
    // stored_bytes leaves out only the repository's tiny `config` file.
    let stored = s["stored_bytes"].as_u64().unwrap();
    assert!(
        stored <= on_disk && on_disk - stored < 1024,
        "{stored} vs {on_disk}"
    );
    assert_eq!(s["snapshots"], 2);
    let latest = s["latest_source_bytes"].as_u64().unwrap();
    assert!(latest >= 4 * MIB as u64);
    let log = env.json(&["log", "disk"]);
    let sum: u64 = log
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["size"].as_u64().unwrap())
        .sum();
    assert_eq!(s["all_snapshots_source_bytes"].as_u64().unwrap(), sum);
    // Two identical snapshots must not cost twice the space.
    assert!(stored < latest + latest / 2);
    assert!(log[0]["added"].as_u64().unwrap() > 0);
    assert!(log[1]["added"].as_u64().unwrap() < 64 * 1024);
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

// ---------------------------------------------------------------------------
// bak-server: the bak client against the bundled server.

struct Server {
    addr: std::net::SocketAddr,
    data: PathBuf,
}

fn start_server(
    env: &Env,
    append_only: bool,
    private_repos: bool,
    tls: bool,
) -> (Server, Option<PathBuf>) {
    let data = env.path("server");
    fs::create_dir_all(&data).unwrap();
    let htpasswd = data.join(".htpasswd");
    bak_server::auth::set_user(&htpasswd, "rob", "rob-server-pw-1").unwrap();
    bak_server::auth::set_user(&htpasswd, "eve", "eve-server-pw-2").unwrap();
    let (tls_config, ca) = if tls {
        let hosts = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let g = bak_server::tls::ensure_generated(&data.join(".tls"), &hosts).unwrap();
        (
            Some(bak_server::tls::server_config(&g.cert, &g.key).unwrap()),
            Some(g.ca),
        )
    } else {
        (None, None)
    };
    let cfg = bak_server::Config {
        path: data.clone(),
        append_only,
        private_repos,
        htpasswd: Some(htpasswd),
        verbose: false,
        dashboard: true,
    };
    let addr = bak_server::spawn("127.0.0.1:0", cfg, tls_config).unwrap();
    (Server { addr, data }, ca)
}

fn login(env: &Env, user: &str, password: &str) {
    env.set_var("BAK_REST_USER", user);
    env.set_var("BAK_REST_PASSWORD", password);
}

/// Minimal HTTP/1.1 client for protocol checks. Returns the status code.
fn http(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    auth: (&str, &str),
    body: &[u8],
) -> u16 {
    use std::net::TcpStream;
    let mut s = TcpStream::connect(addr).unwrap();
    let creds = base64(format!("{}:{}", auth.0, auth.1).as_bytes());
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {creds}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    s.write_all(body).unwrap();
    let mut resp = String::new();
    BufReader::new(s).read_line(&mut resp).unwrap();
    resp.split(' ').nth(1).unwrap().parse().unwrap()
}

fn base64(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in input.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    // A known vector keeps this test free of a hashing dependency.
    assert!(data.is_empty());
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into()
}

// Server 1: round trip and incremental push over HTTP with logins.
#[test]
fn s01_server_round_trip() {
    let env = Env::new();
    let (server, _) = start_server(&env, false, false, false);
    make_fixture(&env.proj, 8, 500);
    env.ok(&["init"]);
    login(&env, "rob", "rob-server-pw-1");
    let url = format!("rest:http://{}/rob/proj", server.addr);
    env.ok(&["remote", "add", "home", &url, "--yes"]);
    let first = env.push("home", "first");
    assert!(first["data_blobs_added"].as_u64().unwrap() > 0);
    let second = env.push("home", "second");
    assert_eq!(second["data_blobs_added"], 0);
    env.ok(&["verify", "home", "--read-data"]);
    let out = env.path("restored");
    env.ok(&["restore", "home", "latest", "--to", out.to_str().unwrap()]);
    assert_same_tree(&env.proj, &out);
    assert!(server.data.join("rob/proj/config").exists());
}

// Server 2: HTTPS with the generated private CA.
#[test]
fn s02_server_tls() {
    let env = Env::new();
    let (server, ca) = start_server(&env, false, false, true);
    let ca = ca.unwrap();
    make_fixture(&env.proj, 1, 50);
    env.ok(&["init"]);
    login(&env, "rob", "rob-server-pw-1");
    let url = format!("rest:https://localhost:{}/rob/proj", server.addr.port());
    // Without the CA the certificate is rejected...
    let out = env.run(&["remote", "add", "home", &url, "--yes"]);
    assert!(!out.status.success(), "untrusted certificate was accepted");
    // ...and with it everything works.
    env.ok(&[
        "remote",
        "add",
        "home",
        &url,
        "--yes",
        "--cacert",
        ca.to_str().unwrap(),
    ]);
    env.push("home", "over tls");
    let out = env.path("restored");
    env.ok(&["restore", "home", "latest", "--to", out.to_str().unwrap()]);
    assert_same_tree_except(&env.proj, &out, &[".bak/config.toml"]);
}

// Server 3: append-only refuses deletes and overwrites but backups still work.
#[test]
fn s03_server_append_only() {
    let env = Env::new();
    let (server, _) = start_server(&env, true, false, false);
    make_fixture(&env.proj, 1, 50);
    env.ok(&["init"]);
    login(&env, "rob", "rob-server-pw-1");
    let url = format!("rest:http://{}/rob/proj", server.addr);
    env.ok(&["remote", "add", "home", &url, "--yes"]);
    env.push("home", "one");
    fs::write(env.proj.join("new.txt"), "two").unwrap();
    env.push("home", "two");
    let auth = ("rob", "rob-server-pw-1");
    let snap = fs::read_dir(server.data.join("rob/proj/snapshots"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .file_name()
        .into_string()
        .unwrap();
    assert_eq!(
        http(
            server.addr,
            "DELETE",
            &format!("/rob/proj/snapshots/{snap}"),
            auth,
            b""
        ),
        403
    );
    assert_eq!(
        http(server.addr, "DELETE", "/rob/proj/config", auth, b""),
        403
    );
    assert_eq!(
        http(server.addr, "POST", "/rob/proj/config", auth, b"overwrite"),
        403
    );
    assert_eq!(env.json(&["log", "home"]).as_array().unwrap().len(), 2);
}

// Server 4: logins and private repositories.
#[test]
fn s04_server_auth() {
    let env = Env::new();
    let (server, _) = start_server(&env, false, true, false);
    make_fixture(&env.proj, 0, 10);
    env.ok(&["init"]);

    // Wrong password: refused, and the password is not echoed.
    login(&env, "rob", "not-the-password-7");
    let url = format!("rest:http://{}/rob/proj", server.addr);
    let out = env.run(&["remote", "add", "home", &url, "--yes"]);
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains("not-the-password-7"), "{all}");

    // eve cannot use rob's area.
    login(&env, "eve", "eve-server-pw-2");
    assert!(!env
        .run(&["remote", "add", "home", &url, "--yes"])
        .status
        .success());
    assert!(!server.data.join("rob").exists());

    // rob can.
    login(&env, "rob", "rob-server-pw-1");
    env.ok(&["remote", "add", "home", &url, "--yes"]);
    env.push("home", "mine");
    assert_eq!(
        http(
            server.addr,
            "GET",
            "/rob/proj/config",
            ("eve", "eve-server-pw-2"),
            b""
        ),
        401
    );
    assert_eq!(
        http(
            server.addr,
            "GET",
            "/rob/proj/config",
            ("rob", "wrong"),
            b""
        ),
        401
    );
    assert_eq!(
        http(
            server.addr,
            "GET",
            "/.htpasswd",
            ("rob", "rob-server-pw-1"),
            b""
        ),
        404
    );
}

// Server 5: uploads are checked against their name, and retries are idempotent.
#[test]
fn s05_server_upload_checks() {
    let env = Env::new();
    let (server, _) = start_server(&env, true, false, false);
    let auth = ("rob", "rob-server-pw-1");
    assert_eq!(
        http(server.addr, "POST", "/rob/x/?create=true", auth, b""),
        200
    );
    let empty = sha256_hex(b"");
    // Correct name: stored. Retried: still OK (append-only does not break retries).
    assert_eq!(
        http(
            server.addr,
            "POST",
            &format!("/rob/x/keys/{empty}"),
            auth,
            b""
        ),
        200
    );
    assert_eq!(
        http(
            server.addr,
            "POST",
            &format!("/rob/x/keys/{empty}"),
            auth,
            b""
        ),
        200
    );
    // Content that does not match its name: rejected, nothing stored.
    let wrong = "1".repeat(64);
    assert_eq!(
        http(
            server.addr,
            "POST",
            &format!("/rob/x/keys/{wrong}"),
            auth,
            b"junk"
        ),
        400
    );
    assert!(!server.data.join(format!("rob/x/keys/{wrong}")).exists());
    // No temp files left behind.
    let leftovers: Vec<_> = fs::read_dir(server.data.join("rob/x/keys"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| n.to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// Server 6: restic can use bak-server too.
#[test]
fn s06_server_restic_client() {
    if !on_path("restic") {
        eprintln!("NOTICE: restic not on PATH; bak-server restic client test skipped");
        return;
    }
    let env = Env::new();
    let (server, _) = start_server(&env, false, false, false);
    make_fixture(&env.proj, 2, 50);
    env.ok(&["init"]);
    login(&env, "rob", "rob-server-pw-1");
    let url = format!("rest:http://{}/rob/proj", server.addr);
    env.ok(&["remote", "add", "home", &url, "--yes"]);
    env.push("home", "for restic");
    let out = env.path("restic-restore");
    let r = Command::new("restic")
        .args([
            "-r",
            &format!("rest:http://rob:rob-server-pw-1@{}/rob/proj", server.addr),
        ])
        .args([
            "--no-cache",
            "restore",
            "latest",
            "--target",
            out.to_str().unwrap(),
        ])
        .env("RESTIC_PASSWORD", PASSPHRASE)
        .output()
        .unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    assert_same_tree(&env.proj, &out);
}

// ---------------------------------------------------------------------------
// bak-server dashboard.

struct HttpResp {
    status: u16,
    headers: String,
    body: String,
}

/// HTTP/1.1 request with optional cookie and JSON body (plain HTTP test server).
fn http_req(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    xbak: bool,
    body: &str,
) -> HttpResp {
    use std::net::TcpStream;
    let mut s = TcpStream::connect(addr).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", body.len());
    if let Some(c) = cookie {
        req += &format!("Cookie: bak_session={c}\r\n");
    }
    if xbak {
        req += "X-Bak: 1\r\n";
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body.as_bytes()).unwrap();
    let mut raw = String::new();
    BufReader::new(s).read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    HttpResp {
        status,
        headers: head.to_ascii_lowercase(),
        body: body.to_string(),
    }
}

fn dash_login(addr: std::net::SocketAddr, user: &str, password: &str) -> String {
    let r = http_req(
        addr,
        "POST",
        "/_/api/login",
        None,
        true,
        &serde_json::json!({ "user": user, "password": password }).to_string(),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let line = r
        .headers
        .lines()
        .find(|l| l.starts_with("set-cookie:"))
        .unwrap()
        .to_string();
    assert!(
        line.contains("httponly") && line.contains("samesite=strict"),
        "{line}"
    );
    line.split("bak_session=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

// Dashboard 1: an admin creates a user, that user backs up, and the dashboard shows it.
#[test]
fn d01_dashboard_flow() {
    let env = Env::new();
    let (server, _) = start_server(&env, true, true, false);
    let addr = server.addr;
    let admins = server.data.join(".admins");
    bak_server::guard::set_admin(&admins, "rob", true).unwrap();

    // No session, or no X-Bak header: refused.
    assert_eq!(
        http_req(addr, "GET", "/_/api/me", None, false, "").status,
        401
    );
    assert_eq!(
        http_req(
            addr,
            "POST",
            "/_/api/login",
            None,
            false,
            r#"{"user":"rob","password":"rob-server-pw-1"}"#
        )
        .status,
        403
    );
    let page = http_req(addr, "GET", "/_/", None, false, "");
    assert_eq!(page.status, 200);
    assert!(page.headers.contains("content-security-policy"));

    let admin = dash_login(addr, "rob", "rob-server-pw-1");
    let me: serde_json::Value =
        serde_json::from_str(&http_req(addr, "GET", "/_/api/me", Some(&admin), false, "").body)
            .unwrap();
    assert_eq!(me["admin"], true);

    // Create a user with a generated password.
    let r = http_req(
        addr,
        "POST",
        "/_/api/users",
        Some(&admin),
        true,
        r#"{"name":"ann","generate":true}"#,
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let created: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    let ann_pw = created["password"].as_str().unwrap().to_string();
    assert_eq!(ann_pw.len(), 20);
    assert_eq!(
        http_req(
            addr,
            "POST",
            "/_/api/users",
            Some(&admin),
            true,
            r#"{"name":"bob","password":"short"}"#
        )
        .status,
        400
    );

    // ann backs up with bak using that password.
    make_fixture(&env.proj, 1, 20);
    env.ok(&["init"]);
    login(&env, "ann", &ann_pw);
    env.ok(&[
        "remote",
        "add",
        "home",
        &format!("rest:http://{addr}/ann/proj"),
        "--yes",
    ]);
    env.push("home", "from ann");

    // ann sees only her repository and cannot manage users.
    let ann = dash_login(addr, "ann", &ann_pw);
    let repos: serde_json::Value =
        serde_json::from_str(&http_req(addr, "GET", "/_/api/repos", Some(&ann), false, "").body)
            .unwrap();
    assert_eq!(repos.as_array().unwrap().len(), 1);
    assert_eq!(repos[0]["path"], "/ann/proj");
    assert_eq!(repos[0]["snapshots"], 1);
    assert!(repos[0]["bytes"].as_u64().unwrap() > 1024 * 1024);
    assert_eq!(
        http_req(addr, "GET", "/_/api/users", Some(&ann), false, "").status,
        403
    );
    assert_eq!(
        http_req(addr, "DELETE", "/_/api/users/rob", Some(&ann), true, "").status,
        403
    );

    // The admin sees the backup in the activity log and overview.
    let act = http_req(addr, "GET", "/_/api/activity", Some(&admin), false, "").body;
    assert!(
        act.contains("\"kind\":\"backup\"") && act.contains("/ann/proj"),
        "{act}"
    );
    let ov: serde_json::Value = serde_json::from_str(
        &http_req(addr, "GET", "/_/api/overview", Some(&admin), false, "").body,
    )
    .unwrap();
    assert_eq!(ov["repos"], 1);
    assert_eq!(ov["users"], 3);

    // Resetting ann's password signs her out of the dashboard and changes her backup login.
    let r = http_req(
        addr,
        "POST",
        "/_/api/users/ann/password",
        Some(&admin),
        true,
        r#"{"generate":true}"#,
    );
    assert_eq!(r.status, 200);
    assert_eq!(
        http_req(addr, "GET", "/_/api/me", Some(&ann), false, "").status,
        401
    );
    assert!(
        !env.run(&["log", "home"]).status.success(),
        "old password still works"
    );

    // Removing ann keeps her repository; the admin cannot remove themselves.
    assert_eq!(
        http_req(addr, "DELETE", "/_/api/users/ann", Some(&admin), true, "").status,
        200
    );
    assert!(server.data.join("ann/proj/config").exists());
    assert_eq!(
        http_req(addr, "DELETE", "/_/api/users/rob", Some(&admin), true, "").status,
        400
    );

    // Logout ends the session.
    assert_eq!(
        http_req(addr, "POST", "/_/api/logout", Some(&admin), true, "").status,
        200
    );
    assert_eq!(
        http_req(addr, "GET", "/_/api/me", Some(&admin), false, "").status,
        401
    );
}

// Dashboard 2: repeated wrong passwords lock the address out, for backup clients too.
#[test]
fn d02_lockout() {
    let env = Env::new();
    let (server, _) = start_server(&env, false, false, false);
    let addr = server.addr;
    for _ in 0..bak_server::guard::MAX_FAILURES {
        assert_eq!(
            http(
                addr,
                "GET",
                "/rob/proj/config",
                ("rob", "wrong-password"),
                b""
            ),
            401
        );
    }
    assert_eq!(
        http(
            addr,
            "GET",
            "/rob/proj/config",
            ("rob", "rob-server-pw-1"),
            b""
        ),
        429
    );
    let r = http_req(
        addr,
        "POST",
        "/_/api/login",
        None,
        true,
        r#"{"user":"rob","password":"rob-server-pw-1"}"#,
    );
    assert_eq!(r.status, 429);
    // Repository names starting with `_` are reserved for the dashboard.
    assert_ne!(
        http(
            addr,
            "POST",
            "/_x/?create=true",
            ("eve", "eve-server-pw-2"),
            b""
        ),
        200
    );
}
