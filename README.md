BAK by Robert Rymarczyk, Licence MIT


**Git-like backups for big project folders.** bak takes versioned, encrypted, deduplicated snapshots of a folder and pushes them to a USB drive, a second disk, an iCloud Drive or Google Drive folder, or your own server. You can browse, diff and restore any snapshot later.

The package has two programs:

- **`bak`**, the command-line client you run on your computer.
- **`bak-server`**, an optional backup server for a machine you control (a home server, a VPS). It is safe to expose to the internet, has automatic HTTPS, user accounts, and a web dashboard.

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
- [bak-server: your own backup server](#bak-server-your-own-backup-server)
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

### Installer scripts

Each release archive (`bak-<version>-<os>-<arch>.tar.gz`) contains `bak`, `bak-server` and an installer. The installers also work from a source checkout, where they build with cargo first.

| System | Command |
| --- | --- |
| macOS | `./installmac.sh` |
| Linux | `./installlinux.sh` |
| Windows | `./installwin.sh`, run inside WSL (see below) |

By default the binaries go to `/usr/local/bin`, which is already on PATH; the installer asks for sudo if it needs it. Options:

| Option | Effect |
| --- | --- |
| `--user` | Install to `~/.local/bin` without sudo, adding it to PATH in your shell profile if needed. |
| `--prefix DIR` | Install to `DIR/bin`. |
| `--client-only` | Install `bak` only. |
| `--uninstall` | Remove the binaries. Backups and repositories are never touched. |
| `--server-service HOST` | Linux only: also run bak-server as a systemd service on port 8000 with TLS, append-only and private repositories, storing data in `/srv/bak`, and print the admin password. |

On macOS, the installer clears the download quarantine flag so Gatekeeper doesn't block the binaries.

**Windows:** bak doesn't run natively on Windows yet. Install WSL (`wsl --install` in an administrator PowerShell, then restart), open Ubuntu, and run `./installwin.sh` there. Windows folders are under `/mnt/c/...`. Run from Git Bash, the script prints these steps.

To build a release archive for the machine you're on: `scripts/package.sh` writes `dist/bak-<version>-<os>-<arch>.tar.gz` and a `.sha256` file.

### Build from source

You need Rust 1.88 or newer ([rustup.rs](https://rustup.rs)).

```sh
git clone <this repository> bak
cd bak
cargo install --path crates/bak-cli      # installs `bak` into ~/.cargo/bin
cargo install --path crates/bak-server   # optional: installs `bak-server`
bak --version
```

To build without installing:

```sh
cargo build --release                     # binaries in target/release/: bak, bak-server
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

# 7. Check the backup can be read, and how much space it uses.
bak verify usb --read-data
bak size usb
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
| `bak log <remote>` | List snapshots: id, time, source size, space the snapshot added at the remote, file count, tags and message. |
| `bak ls <remote> <snapshot> [path]` | List files in a snapshot, optionally under one path. |
| `bak diff <remote> <a> <b>` | Paths added (`+`), removed (`-`) and changed (`M`) between two snapshots. |
| `bak restore <remote> <snapshot> [path] --to <dir>` | Restore a snapshot, or one path from it, into `<dir>`. Refuses a non-empty `<dir>` unless you pass `--force`, which overwrites matching files and never deletes anything. `bak pull` is an alias. |
| `bak verify <remote> [--read-data]` | Check the repository structure. `--read-data` also reads and hashes every pack, which catches bit rot. |
| `bak size [remote]` | Space used at the remote, unique data before and after compression, size of the latest snapshot, and the space saved compared with keeping full copies. Covers all remotes when none is named. |
| `bak copy <from> <to> [snapshot...]` | Copy snapshots (all by default) to another remote without re-reading your files. |

Example `bak size` output:

```
usb  /Volumes/USB1/bak/myproj
  stored at remote      47.7 MiB  in 11 files, 5 packs
  unique data           69.8 MiB  (47.7 MiB after compression)
  latest snapshot       66.8 MiB  3 files
  as full copies       200.3 MiB  (3 snapshots added up)
  space saved          152.6 MiB  (4.2x smaller)
```

"Stored at remote" counts the data packs, index, snapshots and keys, which is what your drive or cloud quota sees (to within a few hundred bytes).

bak finds the project by walking up from the current directory to the nearest `.bak/`, as git does with `.git/`.

These options work with every command:

| Option | Meaning |
| --- | --- |
| `--json` | JSON output for `status`, `log`, `ls`, `diff`, `push`, `verify` and `size`. |
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

The server is [bak-server](#bak-server-your-own-backup-server), or restic's [rest-server](https://github.com/restic/rest-server): both speak the same protocol. `--cacert` takes the CA certificate of a server that uses its own certificates, which is what bak-server does by default.

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

## bak-server: your own backup server

`bak-server` turns any Linux or macOS machine into a backup target for `bak`, and for `restic` and `rustic` too, because it speaks restic's REST protocol. One binary includes:

- **Automatic HTTPS.** On first start it creates its own private certificate authority and a server certificate signed by it, and renews the server certificate on its own. Clients trust the CA once. You can also bring your own certificate (for example from Let's Encrypt).
- **User accounts,** with bcrypt-hashed passwords. The first start creates an `admin` account with a generated password.
- **A web dashboard** at `https://<server>:8000/_/`. See [The dashboard](#the-dashboard).
- **Protection for the open internet:**
  - Plain HTTP is refused on public addresses.
  - 10 failed logins from one IP within 15 minutes lock that IP out for 15 minutes.
  - Paths are parsed strictly.
  - Uploads are checked against their SHA-256 name.
  - Files are never overwritten.
- **Append-only mode,** so a stolen laptop can add backups but never delete or change old ones.
- **Private repositories,** so each user can only reach repositories under `/<username>/`.

The server never sees your data unencrypted. It doesn't have your passphrase, so it can show sizes and times but not file names or contents.

### Quickstart

On the server:

```sh
bak-server serve --path /srv/bak --tls --append-only --private-repos \
  --hostname backup.example.com
```

On first start it prints:

```
 First start: created administrator account
   user:     admin
   password: 4Pw6MzKyXNdK5aVVPZtv
 This is shown only once. ...
clients must trust /srv/bak/.tls/ca.pem
bak-server 0.1.0 serving /srv/bak on https://0.0.0.0:8000
  dashboard: https://<this host>:8000/_/
```

Then:

1. **Sign in to the dashboard** at `https://backup.example.com:8000/_/` as `admin`. Your browser will warn that the certificate is not trusted; see [Certificates](#certificates). Go to **Users**, add an account for yourself, and leave the password empty to have one generated.
2. **On your computer,** download the CA from **Connect a client** (or copy `/srv/bak/.tls/ca.pem`), then:

```sh
export BAK_REST_USER=rob BAK_REST_PASSWORD='<generated password>'
bak remote add home rest:https://backup.example.com:8000/rob/myproj \
  --cacert ~/.config/bak/bak-server-ca.pem
bak push home -m "first backup"
```

### Running on Ubuntu with systemd

```sh
cargo build --release
sudo install -m 755 target/release/bak-server /usr/local/bin/
sudo useradd --system --home /srv/bak --shell /usr/sbin/nologin bak
sudo mkdir -p /srv/bak && sudo chown bak: /srv/bak
```

Create `/etc/systemd/system/bak-server.service`:

```ini
[Unit]
Description=bak backup server
After=network-online.target
Wants=network-online.target

[Service]
User=bak
ExecStart=/usr/local/bin/bak-server serve --path /srv/bak --listen 0.0.0.0:8000 \
  --tls --append-only --private-repos --hostname backup.example.com
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/srv/bak
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now bak-server
sudo journalctl -u bak-server | grep -A3 'First start'    # the admin password, shown once
```

### Exposing it to the internet

- **Forward one TCP port** (8000 in these examples) from your router or cloud firewall to the server. Nothing else needs to be open.
- **Give every name clients use to `--hostname`**, repeating it as needed (for example `--hostname backup.example.com --hostname 203.0.113.7`). The server certificate is only valid for those names. Changing the list creates a new server certificate, signed by the same CA, so clients don't need anything new.
- **Use `--append-only`.** Even if a client machine is compromised, your existing backups can't be deleted or altered through the server.
- **Use `--private-repos`** when more than one person uses the server.
- **Use generated passwords.** They're 20 characters, and a password must be at least 12 characters in any case. The lockout slows down guessing, but strong passwords are what actually stop it.
- **Behind a reverse proxy** (Caddy, nginx) that terminates TLS, run `--listen 127.0.0.1:8000` without `--tls`. The lockout then sees the proxy's address for every client, so prefer bak-server's own TLS when you can.

### Certificates

With `--tls` and no certificate files, bak-server keeps these files in `<path>/.tls/`:

| File | What it is |
| --- | --- |
| `ca.pem` | The private CA certificate. **Give this to clients** (`--cacert`). It is also downloadable from the dashboard. It is valid for 10 years. |
| `ca-key.pem` | The CA's private key (mode 0600). Never share it. |
| `cert.pem`, `key.pem` | The server certificate and its key. They're valid for 397 days, the maximum macOS accepts, and renewed automatically 30 days before expiry. |

**Browsers** don't know your private CA, so the dashboard shows a certificate warning. To remove it, import `ca.pem` into your system or browser certificate store. On macOS: open it in Keychain Access and set *When using this certificate* to *Always Trust*.

**To use your own certificate** instead, for example from Let's Encrypt via certbot, pass `--tls --tls-cert fullchain.pem --tls-key privkey.pem`. Clients then don't need `--cacert`.

### The dashboard

Open `https://<server>:8000/_/` and sign in with a bak-server account.

| Page | What you can do |
| --- | --- |
| Overview | Repositories, stored size, snapshot count, time of the last backup, and backups in the last 24 hours. Admins also see users, failed sign-ins, locked-out IPs and traffic. |
| Repositories | Each repository's path, owner, size, snapshots and packs, with a fresh / old / stale badge based on the last backup. |
| Users (admins) | Add users, with a typed or generated password; reset passwords; grant or revoke admin; remove users. Removing a user keeps their repositories. |
| Activity | Backups, repositories created, deletions, refusals in append-only mode, sign-ins, failed sign-ins, lockouts and user changes. |
| Connect a client | The CA download and copy-paste `bak` commands for your account. |
| Account | Change your own password, which needs your current one. |

Regular users see only their own repositories and activity. Sessions last 12 hours, use HttpOnly SameSite=Strict cookies, and are signed out when the password changes. The dashboard never deletes repositories.

### Managing users from the command line

```sh
bak-server user add rob --path /srv/bak --generate          # prints a generated password
bak-server user add alice --path /srv/bak --admin           # asks for a password
bak-server user list --path /srv/bak
bak-server user remove rob --path /srv/bak                  # keeps rob's repositories
```

Users live in `<path>/.htpasswd`, in standard bcrypt htpasswd format, so `htpasswd -B` works too. Admins are listed in `<path>/.admins`.

### `bak-server serve` options

| Option | Meaning |
| --- | --- |
| `--path <dir>` | Where repositories live (required). |
| `--listen <addr>` | Address and port to listen on. Default `0.0.0.0:8000`. |
| `--tls` | Serve HTTPS. Generates certificates in `<path>/.tls` unless `--tls-cert` and `--tls-key` are given. |
| `--hostname <name>` | A name or IP that clients use (repeatable). Used for generated certificates. |
| `--append-only` | Refuse deletes (except lock files) and overwrites. |
| `--private-repos` | Each user may only use `/<username>/…`. |
| `--htpasswd <file>` | Users file (default `<path>/.htpasswd`). |
| `--no-auth` | No logins, and no dashboard. Only for a trusted local network. |
| `--no-dashboard` | Don't serve `/_/`. |
| `--allow-http` | Allow plain HTTP on a non-loopback address, for use behind a TLS proxy or VPN. |
| `-v`, `--verbose` | Log every request. |

### Using restic's rest-server instead

bak also works with restic's [rest-server](https://github.com/restic/rest-server) (`--append-only`, `--private-repos`, `--tls`, an htpasswd file). It has no dashboard and no automatic certificates.

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
- **No FUSE mount, no desktop GUI, no Windows.** The web dashboard belongs to bak-server and shows server-side information only.
- **bak-server's activity log and sessions live in memory,** so they reset when the server restarts.

## Development

The workspace has four crates. Only `bak-engine-rustic` depends on the backup library, so the engine can be replaced without touching the command line.

| Crate | Role |
| --- | --- |
| [`crates/bak-core`](crates/bak-core) | Config, remotes, mount guard, secret redaction, snapshot diff, and the `Engine` trait. No library types in its API. |
| [`crates/bak-engine-rustic`](crates/bak-engine-rustic) | The `Engine` implementation on rustic_core and rustic_backend, which are pinned to exact versions. |
| [`crates/bak-cli`](crates/bak-cli) | The `bak` binary. Every command is written against `&dyn Engine`; `main.rs` is the only file that picks the engine. |
| [`crates/bak-server`](crates/bak-server) | The `bak-server` binary: the restic REST protocol on hyper and rustls, htpasswd and bcrypt logins, generated certificates, lockout, and the dashboard (plain HTML, CSS and JS in `assets/`, compiled into the binary). |

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
- size
- bak-server: round trip, HTTPS with the generated CA, append-only, logins and private repositories, upload checks, and restic as a client
- the dashboard: user creation, a backup by that user, visibility rules, password reset, removal, logout, and lockout

They run only in temporary directories. The bak-server tests start the server inside the test process. The restic interop and rest-server tests run when `restic` and `rest-server` are on your PATH, and are otherwise skipped with a notice.

Design notes are in [`docs/DECISIONS.md`](docs/DECISIONS.md). [`docs/SPIKE-REPORT.md`](docs/SPIKE-REPORT.md) evaluates rustic_core as the engine.

`unsafe` code is forbidden throughout the workspace.

## License

MIT. See [LICENSE](LICENSE).

bak depends on [rustic_core](https://github.com/rustic-rs/rustic_core) and [rustic_backend](https://github.com/rustic-rs/rustic_core), which are dual-licensed Apache-2.0 / MIT, and writes the [restic](https://restic.net) repository format.
