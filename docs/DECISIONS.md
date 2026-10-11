# Decisions

Choices made while building bak v0.1, where the SOW was silent or had an open question.

## Pinned versions

| Crate | Version | Notes |
| --- | --- | --- |
| `rustic_core` | `=0.13.0` | Latest on crates.io on 2026-10-11. |
| `rustic_backend` | `=0.7.0` | Latest; depends on `rustic_core` 0.13. Built with `default-features = false, features = ["rest"]`. That drops OpenDAL and rclone, which bak does not use, and removes about 100 crates from the tree. |
| `bytesize` | `2.3.1` | Already in the tree through `rustic_core`. Added as a direct dependency only to name the `ByteSize` type in `ConfigOptions`. |

Toolchain: rustc 1.97.0. `rustic_core` 0.13 uses let-chains, so it needs a recent compiler.

## Open questions

| Question | Resolution |
| --- | --- |
| Server transport | REST via `rest-server` (default). |
| Honour `.gitignore`? | No. Only `.bakignore` excludes files, and `.git` is backed up. Checked by experiment: a global `core.excludesFile` is not applied either. |
| Is macOS a target? | **Yes, primary.** The owner works on a Mac and wants iCloud Drive and Google Drive as destinations. Built and tested on macOS 26 (arm64). Linux must still compile, and `rest-server` is expected on Ubuntu. |
| Repository per project or shared? | One per project. |
| Licence | **MIT** (decided 2026-10-11, for publishing on GitHub). See `LICENSE`. |

## Choices where the SOW was silent

- **`bak init` tracks `.`** so that `init`, `remote add`, `push` works with no extra step. `bak add` and `bak untrack` refine the list.
- **Snapshot identity.** Snapshots record source paths relative to the project root and use the project name as the host name. rustic chooses the parent snapshot by host, label and paths, so incremental pushes survive moving the project or renaming the machine. The working directory is set to the project root during a push (see the friction list in the spike report).
- **`.bakignore` semantics.** It is applied as a custom ignore file name, so a `.bakignore` in any directory applies to that directory and below, like `.gitignore`. This is a superset of "at the project root".
- **Restore.** Ownership is not restored, because that needs root. Content, modes, timestamps and symlinks are. `--force` overwrites matching files and never deletes. A sub-path is restored at the same relative location under `--to`.
- **Passphrase order.** `--password-file` (must be mode 0600), then `BAK_PASSWORD`, then a prompt. One passphrase per project, used for every remote; `copy` relies on this.
- **REST credentials.** `BAK_REST_USER_<REMOTE>` / `BAK_REST_PASSWORD_<REMOTE>`, then `BAK_REST_USER` / `BAK_REST_PASSWORD`, then the remote's `credentials_file` (a `user:password` line, mode 0600). A URL with credentials in it is rejected. Both username and password are registered for redaction.
- **Redaction.** Every secret is registered in a process-wide list (`bak-core/src/redact.rs`), in raw and URL-encoded form. All log lines and error messages go through it.
- **Mount guard.** A path is "mounted" if its nearest existing ancestor is on a device other than `/` and, on macOS, `/System/Volumes/Data`. Applied to `remote add` before creating anything, to `push`, and to the destination of `copy`.
- **Cloud folders.** A local path under `~/Library/Mobile Documents/` (iCloud Drive), `~/Library/CloudStorage/` (Google Drive, OneDrive, Dropbox), or a folder named `Google Drive`, `Dropbox` or `OneDrive` gets 128 MiB data packs unless `--pack-size` says otherwise.
- **Chunker parameters.** `remote add` copies the chunker parameters of the first reachable existing remote, so `copy` between them deduplicates.
- **Engine boundary.** `bak-cli` is a library holding every command, written against `&dyn Engine`, plus `main.rs`, which is the only file that names `RusticEngine`. Swapping engines changes one line in `main.rs` and the dependency in `Cargo.toml`.
- **Engine cache.** rustic's cache goes to the platform cache directory. `BAK_CACHE_DIR` overrides it, and the tests always set it to a temporary directory.
- **Progress.** A terminal gets indicatif bars. Otherwise bak prints `progress: <title> <n>%` lines at 10% steps, which test 7 uses to interrupt a push at a deterministic point.
- **Milestones.** The work was done in one session, so it is one commit, not five.
- **Logging crate.** rustic logs through `log`, so bak uses `log` plus `env_logger`.

## bak-server (added 2026-10-11)

The SOW originally ruled out a custom server in favour of restic's `rest-server`. The owner changed that: the project now ships its own server, so the package is complete without a Go binary.

- **Protocol.** restic's REST protocol, v1 and v2 listings, so `bak`, `restic` and `rustic` all work against it. Repositories are plain restic directories on disk, so a repository can move between bak-server and rest-server unchanged.
- **Stack.** hyper and tokio directly, not axum: the routing is one strict path parser. TLS uses rustls with the aws-lc-rs provider, which reqwest already pulls in. New dependencies: `bcrypt` (htpasswd) and `rcgen` (certificates). `tokio-rustls`, `sha2`, `base64` and `tokio-util` were already in the tree.
- **Certificates.** A generated private CA plus a server certificate it signs, instead of one self-signed certificate. macOS's certificate checker, which the bak client uses, is strict about self-signed leaf certificates. A CA also lets the server certificate be renewed (397-day validity, the macOS maximum) without clients changing anything. The CA's parameters are rebuilt deterministically, so renewal needs only the stored CA key and no X.509 parser.
- **Writes.** Uploads are written to a temporary file, fsynced, checked against their SHA-256 name, and hard-linked into place. A hard link never replaces an existing file. A retried upload with identical content succeeds, which keeps append-only mode compatible with client retries.
- **Internet exposure.**
  - Plain HTTP on a non-loopback address is refused unless `--allow-http` is given.
  - Ten failed logins from one IP within 15 minutes lock it out for 15 minutes. This covers backup clients and the dashboard.
  - Passwords must be at least 12 characters, and generated ones are 20.
  - The server binds its port and loads TLS before creating the first-run admin, so a failed start never prints a password that then goes unused.
- **Dashboard.**
  - Static HTML, CSS and JS compiled into the binary, with no CDN and no framework.
  - A strict Content-Security-Policy with no inline scripts.
  - Session cookies are HttpOnly, SameSite=Strict, and Secure under TLS. Mutating calls must carry an `X-Bak: 1` header.
  - The `_` prefix is reserved for the dashboard, so repository and user names can't start with it.
  - The dashboard can't delete repositories.
- **Roles.** Admin names live in `.admins` next to the htpasswd file, because htpasswd has no place for roles.
- **Not persisted.** The activity log and sessions are in memory, so a restart clears them. That's acceptable for v0.1.
