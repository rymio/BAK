BAK by Robert Rymarczyk, Licence MIT


**Git-like backups for big project folders.** bak takes versioned, encrypted, deduplicated snapshots of a folder and pushes them to a USB drive, a second disk, an iCloud Drive or Google Drive folder, or your own server. You can browse, diff and restore any snapshot later.

```sh
bak init
bak remote add usb /Volumes/USB1/bak/myproj --require-mount
bak push usb -m "before the big refactor"
bak log usb
bak restore usb latest --to ~/restore
```

bak was built for large git repositories that are too big for GitHub or Bitbucket, but it works for any folder.

> **Status: v0.1, early.** The core loop (push, log, ls, diff, restore, verify, copy) works and is covered by an acceptance test suite. It hasn't yet been tested on a real 10 GB+ repository. Keep a second backup of anything you can't afford to lose until you have run your own restore test.

## Contents

- [Why bak](#why-bak)
- [How it works](#how-it-works)
- [Install](#install)
- [Quickstart](#quickstart)
- [Commands](#commands)
- [Remotes](#remotes)
- [Configuration](#configuration)
- [Passphrases and credentials](#passphrases-and-credentials)
- [Running a backup server on Ubuntu](#running-a-backup-server-on-ubuntu)
- [Restoring without bak](#restoring-without-bak)
- [Limitations](#limitations)
- [Development](#development)
- [License](#license)

## Why bak

- **Familiar commands.** `push`, `log`, `ls`, `diff` and `restore` instead of a page of backup flags.
- **Only changes are stored.** Files are split into content-defined chunks, so changing 1 MiB in the middle of a 200 MB file stores about 3.5 MB, not 200 MB. A push with no changes stores nothing new.
- **Encrypted at the source.** A drive, cloud account or server only ever sees encrypted data.
- **Open format.** A bak repository is a standard [restic](https://restic.net) repository. If bak disappears, `restic` can still read every backup.
- **Safe by default.**
  - bak never deletes backup data: there is no `forget` and no `prune`.
  - A mount guard stops an unplugged USB drive from turning into a backup on your internal disk.
  - Passphrases and server credentials never appear in config files or logs.
- **Friendly to cloud folders.** Data is stored in a few large, immutable files, which is what iCloud Drive and Google Drive sync best.

## How it works

A **project** is a folder with a `.bak/config.toml` file in it. The config lists which paths are **tracked** and which **remotes** to back up to.

A **remote** is a name plus a location. Each remote holds one **repository**, in restic's format, which stores many **snapshots** of the project.

`bak push <remote>` reads the tracked paths and splits files into chunks. It encrypts any chunk the repository doesn't already have, bundles new chunks into pack files, and records a snapshot. There is no local staging area: the remote is the history.

```
 your project ──push──▶ remote "usb"     /Volumes/USB1/bak/myproj      (restic repository)
              ──push──▶ remote "icloud"  ~/Library/Mobile Documents/…  (restic repository)
              ──push──▶ remote "home"    rest:https://server:8000/…    (restic repository)
```

Under the hood bak uses [rustic_core](https://github.com/rustic-rs/rustic_core), a Rust implementation of the restic format.

## Install

### Build from source

You need Rust 1.88 or newer ([rustup.rs](https://rustup.rs)).

```sh
git clone <this repository> bak
cd bak
cargo install --path crates/bak-cli      # installs `bak` into ~/.cargo/bin
bak --version
```

To build without installing:

```sh
cargo build --release                     # binary at target/release/bak
```

### Platforms

| Platform | Status |
| --- | --- |
| macOS (Apple silicon and Intel) | Primary. Developed and tested here. |
| Linux | Builds. Most tests were run on macOS. |
| Windows | Not supported. |

## Quickstart

```sh
cd ~/code/myproj

# 1. Make this folder a bak project. By default it tracks the whole folder.
bak init

# 2. Optionally exclude files, in gitignore syntax.
#    .gitignore is NOT used: bak backs up .git and everything else unless .bakignore says otherwise.
printf 'target/\nnode_modules/\n*.tmp\n' > .bakignore

# 3. Add a remote. bak offers to create a repository there and asks for a passphrase.
bak remote add usb /Volumes/USB1/bak/myproj --require-mount

# 4. Back up.
bak push usb -m "first backup" --tag v1

# 5. Look around.
bak log usb                       # list snapshots
bak ls usb latest src/            # list files in a snapshot
bak diff usb 0e423ab2 latest      # what changed between two snapshots

# 6. Get files back.
bak restore usb latest --to ~/restore              # whole snapshot
bak restore usb 0e423ab2 src/lib --to ~/restore    # one directory from an older snapshot

# 7. Check the backup can be read.
bak verify usb --read-data
```

**Keep your passphrase somewhere safe.** Without it the backups cannot be decrypted, by bak or by anyone else.

## Commands

A `<snapshot>` is a short id from `bak log` (such as `0e423ab2`) or `latest`.

| Command | What it does |
| --- | --- |
| `bak init` | Create `.bak/config.toml` in the current directory, tracking `.`. Refuses if a project already exists. |
| `bak add <path>...` | Track more paths. Paths are stored relative to the project root. |
| `bak untrack <path>...` | Stop tracking paths. Backup data is not touched. |
| `bak status` | Tracked paths, remotes, and the latest snapshot on each reachable remote. |
| `bak remote add <name> <url>` | Register a remote. If there's no repository at the URL, offers to create one (`--yes` skips the question). |
| `bak remote list` | List remotes. |
| `bak remote remove <name>` | Unregister a remote. Data at the remote is never deleted. |
| `bak push <remote> [-m <msg>] [--tag <tag>]...` | Snapshot all tracked paths. `--all` pushes to every remote in turn. |
| `bak log <remote>` | List snapshots: id, time, size, file count, tags and message. |
| `bak ls <remote> <snapshot> [path]` | List files in a snapshot, optionally under one path. |
| `bak diff <remote> <a> <b>` | Paths added (`+`), removed (`-`) and changed (`M`) between two snapshots. |
| `bak restore <remote> <snapshot> [path] --to <dir>` | Restore a snapshot, or one path from it, into `<dir>`. Refuses a non-empty `<dir>` unless you pass `--force`, which overwrites matching files and never deletes anything. `bak pull` is an alias. |
| `bak verify <remote> [--read-data]` | Check the repository structure. `--read-data` also reads and hashes every pack, which catches bit rot. |
| `bak copy <from> <to> [snapshot...]` | Copy snapshots (all by default) to another remote without re-reading your files. |

bak finds the project by walking up from the current directory to the nearest `.bak/`, as git does with `.git/`.

These options work with every command:

| Option | Meaning |
| --- | --- |
| `--json` | JSON output for `status`, `log`, `ls`, `diff`, `push` and `verify`. |
| `--password-file <file>` | Read the passphrase from a file (must be mode 0600). |
| `-v`, `-vv` | Info or debug logging to stderr. Secrets are always masked. |

The exit code is 0 on success and non-zero on any failure, with the reason on stderr. On a terminal, long operations show progress bars. Otherwise bak prints plain `progress: … 50%` lines.

## Remotes

The URL decides what kind of remote it is.

| Kind | URL example |
| --- | --- |
| USB drive | `/Volumes/USB1/bak/myproj` (Linux: `/media/$USER/USB1/bak/myproj`) |
| Second disk | `/Volumes/Backup/bak/myproj` or `/mnt/backup/bak/myproj` |
| iCloud Drive | `~/Library/Mobile Documents/com~apple~CloudDocs/bak/myproj` |
| Google Drive | `~/Library/CloudStorage/GoogleDrive-you@example.com/My Drive/bak/myproj` |
| Backup server | `rest:https://backup.example.internal:8000/myproj` |

Use one repository per project. A path may start with `~/`. Use `local:<path>` for an unusual path.

### USB drives and the mount guard

```sh
bak remote add usb /Volumes/USB1/bak/myproj --require-mount
```

With `--require-mount`, bak refuses to write unless the path is on a mounted drive and not on the system disk. Without it, pushing while the drive is unplugged could quietly create a backup on your internal disk under `/Volumes/USB1`. The guard applies to creating the repository, to `push`, and to the destination of `copy`.

### iCloud Drive and Google Drive

Sync clients choke on folders with hundreds of thousands of small files. A bak repository doesn't look like that: data is bundled into large pack files, and for cloud-synced folders bak uses 128 MiB packs automatically (set your own with `--pack-size <MiB>`). In a test, 3,001 source files (1 GB) became **13 files** in the repository. A push only ever adds new files and never edits existing ones.

```sh
bak remote add icloud "~/Library/Mobile Documents/com~apple~CloudDocs/bak/myproj"
bak remote add gdrive "~/Library/CloudStorage/GoogleDrive-you@example.com/My Drive/bak/myproj"
```

To use these well:

- **Keep the folder downloaded.** Mark it *Keep Downloaded* (iCloud) or *Available offline* (Google Drive). If the client evicts pack files to save space, `restore` and `verify --read-data` stall while it downloads them again.
- **Push from one machine per repository.** Two machines writing the same synced folder at once can create conflict copies.
- **A backup counts once it's uploaded.** `bak push` finishes when the files are in the local folder. Wait for the sync client to finish before treating the backup as off-site.
- The mount guard doesn't apply here, because these folders live on the system disk.

### Backup server

```sh
export BAK_REST_USER=rob BAK_REST_PASSWORD='…'
bak remote add home rest:https://backup.example.internal:8000/rob/myproj \
  --cacert ~/.config/bak/home-ca.pem
```

The server is restic's [rest-server](https://github.com/restic/rest-server). See [Running a backup server on Ubuntu](#running-a-backup-server-on-ubuntu). Use `--cacert` when the server has a self-signed certificate.

### Copying between remotes

```sh
bak copy usb icloud            # every snapshot on usb that icloud lacks
bak copy usb home 0e423ab2     # one snapshot
```

When `remote add` creates a repository, it reuses the chunking parameters of an existing remote of the project, so copied snapshots deduplicate at the destination.

## Configuration

`.bak/config.toml` is plain TOML and safe to edit. It never contains secrets.

```toml
[project]
name = "myproj"          # also used to match snapshots across machines and renames
tracked = ["."]          # paths relative to the project root

[remotes.usb]
url = "/Volumes/USB1/bak/myproj"
require_mount = true

[remotes.icloud]
url = "~/Library/Mobile Documents/com~apple~CloudDocs/bak/myproj"
pack_size_mib = 128      # only used when the repository is created

[remotes.home]
url = "rest:https://backup.example.internal:8000/rob/myproj"
cacert = "~/.config/bak/home-ca.pem"
credentials_file = "~/.config/bak/home.credentials"   # optional, see below
```

**`.bakignore`** uses gitignore syntax. A `.bakignore` in any directory applies to that directory and below. `.gitignore` files are not used.

| Environment variable | Purpose |
| --- | --- |
| `BAK_PASSWORD` | Repository passphrase. |
| `BAK_REST_USER`, `BAK_REST_PASSWORD` | Server credentials, for all REST remotes. |
| `BAK_REST_USER_<NAME>`, `BAK_REST_PASSWORD_<NAME>` | Server credentials for the remote `<name>`, written in upper case (for example `BAK_REST_PASSWORD_HOME`). These take precedence. |
| `BAK_CACHE_DIR` | Where to keep the local metadata cache. Defaults to the platform cache directory. |

## Passphrases and credentials

**Repository passphrase.** bak reads it from, in order:

1. `--password-file <file>` (the file must be mode 0600)
2. the `BAK_PASSWORD` environment variable
3. an interactive prompt

Use one passphrase for all remotes of a project. bak never writes it to disk, logs or command-line arguments.

**Server credentials.** bak reads them from the environment variables above, or from a `credentials_file` holding a single `username:password` line with mode 0600. A remote URL that contains credentials is rejected.

**Redaction.** Every secret is registered when it's read. Every log line and error message is scrubbed of secrets before printing, including errors raised inside the backup library.

## Running a backup server on Ubuntu

bak talks to restic's `rest-server`. bak doesn't set the server up for you; these are the manual steps.

```sh
# 1. Install. Download the latest linux_amd64 release from
#    https://github.com/restic/rest-server/releases and unpack it.
sudo install -m 755 rest-server /usr/local/bin/rest-server
sudo useradd --system --home /srv/restic --shell /usr/sbin/nologin restic
sudo mkdir -p /srv/restic && sudo chown restic: /srv/restic

# 2. Create a user (bcrypt).
sudo apt install apache2-utils
sudo htpasswd -B -c /srv/restic/.htpasswd rob

# 3. TLS. A self-signed certificate is fine: copy cert.pem to your Mac and pass it with --cacert.
sudo openssl req -x509 -newkey rsa:4096 -nodes -days 3650 \
  -keyout /srv/restic/key.pem -out /srv/restic/cert.pem \
  -subj "/CN=backup.example.internal" \
  -addext "subjectAltName=DNS:backup.example.internal"
sudo chown restic: /srv/restic/*.pem
```

Create `/etc/systemd/system/rest-server.service`:

```ini
[Unit]
Description=restic rest-server
After=network.target

[Service]
User=restic
ExecStart=/usr/local/bin/rest-server --path /srv/restic --listen :8000 \
  --append-only --private-repos \
  --tls --tls-cert /srv/restic/cert.pem --tls-key /srv/restic/key.pem
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now rest-server
```

- `--append-only` lets clients add backups but never delete or overwrite them, so a compromised laptop cannot wipe your history.
- `--private-repos` limits each user to repositories under `/<username>/`, which is why the URL above is `…:8000/rob/myproj`.

## Restoring without bak

Every bak repository is a normal restic repository. Install [restic](https://restic.net) (`brew install restic` or `sudo apt install restic`) and use the same passphrase:

```sh
restic -r /Volumes/USB1/bak/myproj snapshots
restic -r /Volumes/USB1/bak/myproj restore latest --target ~/restore
restic -r "rest:https://rob:PASSWORD@backup.example.internal:8000/rob/myproj" \
  --cacert home-ca.pem restore latest --target ~/restore
```

## Limitations

- **No deletion.** Repositories only grow. Pruning old snapshots is deliberately out of scope for v0.1.
- **Consistency.** bak reads files while they may be changing. Pushing during a git commit, rebase or gc can capture an inconsistent `.git`. Push when the repository is idle.
- **Ownership** (user and group) is not restored. File contents, permissions, timestamps and symlinks are.
- **Paths.** Remote and restore paths must be valid UTF-8. File names inside your project may be anything your filesystem allows.
- **No scheduling.** bak doesn't run in the background or on a timer. Use cron or launchd if you want that.
- **No FUSE mount, no GUI, no Windows.**

## Development

The workspace has three crates. Only one of them depends on the backup library, so the engine can be replaced without touching the command line.

| Crate | Role |
| --- | --- |
| [`crates/bak-core`](crates/bak-core) | Config, remotes, mount guard, secret redaction, snapshot diff, and the `Engine` trait. No library types in its API. |
| [`crates/bak-engine-rustic`](crates/bak-engine-rustic) | The `Engine` implementation on rustic_core and rustic_backend, which are pinned to exact versions. |
| [`crates/bak-cli`](crates/bak-cli) | The `bak` binary. Every command is written against `&dyn Engine`; `main.rs` is the only file that picks the engine. |

```sh
scripts/check.sh                  # cargo fmt --check, clippy -D warnings, all tests
cargo test                        # unit and acceptance tests (about a minute)
BAK_TEST_BIG_MB=20 cargo test     # faster: smaller large-file fixture
```

The acceptance tests in [`crates/bak-cli/tests/acceptance.rs`](crates/bak-cli/tests/acceptance.rs) cover:

- round trip
- incremental push
- deduplication
- history
- sub-path restore
- corruption detection
- interrupted push
- restic interop
- REST server
- the mount guard
- secret redaction
- copy

They run only in temporary directories. The restic interop and REST tests run when `restic` and `rest-server` are on your PATH, and are otherwise skipped with a notice.

Design notes are in [`docs/DECISIONS.md`](docs/DECISIONS.md). [`docs/SPIKE-REPORT.md`](docs/SPIKE-REPORT.md) evaluates rustic_core as the engine.

`unsafe` code is forbidden throughout the workspace.

## License

MIT. See [LICENSE](LICENSE).

bak depends on [rustic_core](https://github.com/rustic-rs/rustic_core) and [rustic_backend](https://github.com/rustic-rs/rustic_core), which are dual-licensed Apache-2.0 / MIT, and writes the [restic](https://restic.net) repository format.
