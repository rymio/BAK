# Spike report: bak v0.1 on rustic_core

**Question:** is rustic_core good enough to keep building on, or should the engine be written from scratch? A third option is shelling out to `restic`.

**Short answer:** keep rustic_core, provisionally. Every `Engine` operation works without forking or patching the library, and all three workarounds stay inside `bak-engine-rustic`. Three of the five decision criteria still need measurements that could not be taken on this machine (see the criteria table below).

## Engine operations

| Operation | Mapping | Notes |
| --- | --- | --- |
| Open or create | Clean | `Repository::new_with_progress` → `open` / `init`. Backends are built directly (`LocalBackend::new`, `RestBackend::new`), not from a URL string. |
| Snapshot | Workaround | `Repository::backup`. Relative source paths resolve against the process working directory, so the adapter must `chdir` to the project root. |
| List snapshots | Clean | `get_all_snapshots`. |
| List entries | Clean | `node_from_snapshot_and_path` + `ls`. |
| Restore | Clean | `prepare_restore` + `restore`, with the same `ls` stream. |
| Verify | Workaround | `check`. The error level type is not exported. |
| Copy | Clean, one workaround to share chunker parameters | `relevant_copy_snapshots` on the destination, then `copy`. Creating a repository that shares chunker parameters needs a serde round trip. |
| Diff | Not in the library | Built in `bak-core/src/diff.rs` from two listings, as the SOW allowed. |

## API friction

Line numbers are in the pinned sources (`~/.cargo/registry/src/*/rustic_core-0.13.0`, `rustic_backend-0.7.0`).

1. **Backup paths depend on the process working directory.** Relative paths are stored verbatim (`rustic_core/src/commands/backup.rs:261`) and walked relative to the working directory. `as_path` only works for a single path. Workaround: `std::env::set_current_dir` in `crates/bak-engine-rustic/src/lib.rs:144`. This is process-global, so the engine is not safe to run concurrently in one process.
2. **`CheckErrorLevel` is not re-exported.** `CheckResults` exposes `Vec<(CheckErrorLevel, CheckError)>`, but `CheckErrorLevel` (`rustic_core/src/commands/check.rs:960`) is missing from the re-exports (`rustic_core/src/lib.rs:156`). Workaround: compare its `Debug` output (`crates/bak-engine-rustic/src/lib.rs:275`). This breaks silently if the variant is renamed.
3. **A repository id can't be built.** `ConfigFile` is public, but its `id: RepositoryId` lives in a `pub(crate)` module (`rustic_core/src/repofile.rs:12`). Creating a repository with the same chunker parameters as another (restic's `--copy-chunker-params`) therefore needs a serde round trip to set a fresh id (`crates/bak-engine-rustic/src/lib.rs:319`).
4. **Backend errors echo credentials.** `RestBackend::new` attaches the full URL, including `user:password@`, to its parse error (`rustic_backend/src/rest.rs:157`). `BackendOptions::to_backends` does the same with the location (`rustic_backend/src/choose.rs:123`). `location()` masks only the password and still logs the user name (`rustic_backend/src/rest.rs:252`). Workaround: process-wide redaction of every log line and error. Test 11 forces this error path.
5. **Local paths must be UTF-8 strings.** `LocalBackend::new` takes `impl AsRef<str>` (`rustic_backend/src/local.rs:47`), and so does `LocalDestination::new` (`rustic_core/src/backend/local_destination.rs:167`). A repository or restore target with a non-UTF-8 path is rejected with a clear error.
6. **URL strings select backends by the text before the first `:`** (`rustic_backend/src/util.rs:60`), so a local path containing `:` would be misread. bak avoids this by building backends directly.
7. **Pack size needs `bytesize::ByteSize`.** `ConfigOptions::set_datapack_size` takes a type from a crate rustic does not re-export, so bak adds `bytesize` as a direct dependency at the same version.

Checked and not an issue: a global git `core.excludesFile` is not applied. The walker is built with `.ignore(false)` and git-ignore off (`rustic_core/src/backend/ignore.rs:151`), confirmed by experiment.

## Measurements

Machine: macOS 26 on arm64. Release build. Measured on 2026-10-11.

| Run | Source | First push | Peak memory | Repository files |
| --- | --- | --- | --- | --- |
| Synthetic, cloud-folder remote, 128 MiB packs | 1 GB random file + 3,000 × 2 KB files | 2.7 s | 312 MB | 13 files (9 data packs, largest 133 MB) |
| Dedup (test 3) | 1 MiB overwrite in a 200 MB file | — | — | 3.5 MB added |

**Manual 10 GB run: not done yet.** The SOW requires a real repository of at least 10 GB on each remote kind, compared with the `restic` CLI. That needs your real drives, the Ubuntu server, and `restic` installed, so it is left for you. Record the results in this table:

| Remote | First push | Second push | Repository / source size | Peak memory | restic first / second push |
| --- | --- | --- | --- | --- | --- |
| Local disk | | | | | |
| USB drive | | | | | |
| REST server | | | | | |
| iCloud / Google Drive folder | | | | | |

Use `/usr/bin/time -l bak push <remote>` on macOS (or `/usr/bin/time -v` on Linux) for time and peak memory.

## Dependencies and audit

- 358 packages in `Cargo.lock`, and 254 unique crates in the normal (non-dev) dependency tree. Disabling `rustic_backend`'s OpenDAL and rclone features keeps it this low.
- Release binary: 13.6 MB.
- **`cargo audit` and `cargo deny check`: not run.** Neither tool is installed, and installing them writes outside the project. To run them: `cargo install cargo-audit cargo-deny && cargo audit && cargo deny init && cargo deny check`.

## Acceptance tests

All 13 tests in `crates/bak-cli/tests/acceptance.rs` pass: the 12 from the SOW plus a `copy` test. The full suite, with the 200 MB file and 20,000 small files, takes about 65 s.

- **Test 8 (restic interop) and test 9 (REST)** were skipped with a notice, because `restic` and `rest-server` are not on PATH here. They run automatically once installed (`brew install restic rest-server`).
- **The non-UTF-8 file name case** is skipped with a notice on macOS, because APFS refuses such names. It runs on Linux.
- **Test 11 takes about 60 s,** because the REST backend retries a refused connection with backoff before giving up.
- **Test 12:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass (`scripts/check.sh`). `unsafe_code = "forbid"` is set for the whole workspace.

## Decision criteria

| Criterion | Status |
| --- | --- |
| Every operation without forking or patching; workarounds only in the adapter | **Pass** |
| Push times within 1.5× of the `restic` CLI | **Pending.** Needs the manual run with `restic` installed. |
| Peak memory under 1 GB on a 10 GB push | **Pending.** 312 MB at 1 GB of source. rustic's index memory grows with repository size, so 10 GB needs measuring. |
| No unresolved friction blocks a planned v0.2 feature | **Pass.** Item 1 matters only if v0.2 runs pushes in parallel within one process. A per-push subprocess or an upstream `as_path`-style fix would solve it. |
| `cargo audit` clean | **Pending.** Tools not installed. |

## Recommendation

Keep building on rustic_core, conditional on the three pending criteria passing.

- **Against writing our own engine:** the repository format, chunker, crypto, pack layout, check, copy and restore planning all work here, and they interoperate with restic. Reproducing them is months of work, plus a long tail of correctness risk.
- **Against shelling out to `restic`:** it would avoid friction items 1–3 and 7, but it adds a runtime dependency that must be installed and version-matched, process and JSON parsing at every call, and no typed errors.

Each friction item is small, and all of them are contained in a ~400-line adapter. Items 2 and 3 are worth reporting upstream as missing re-exports.

**Revisit this if** the 10 GB run breaks the memory or speed criteria, or if a rustic_core upgrade breaks the adapter in a way that costs more than a day.
