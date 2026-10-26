# Vendored code

Code copied into this repository from another project has one row here, giving where it lives, where it came from, the upstream commit it matches, its licence and where the upstream notice is kept.

Two more pieces are planned:

- the treemap palette tables (`crates/duscope-treemap/src/packs.rs`, P1-MAP-02)
- the Inter font subset (OFL-1.1, with its licence file beside it, P2-MAP-02)

| Path | Upstream | Commit | Licence | Notice | Synced |
| --- | --- | --- | --- | --- | --- |
| `crates/duscope-walk/src/scheduler.rs` | [Byron/dua-cli](https://github.com/Byron/dua-cli), `crates/dua-lib/src/lib.rs` (dua-core 5.0.1): the worker pool, which is `PoolShared`, `Pool`, `start_pool`, `worker_loop`, `find_job`, `schedule_jobs` and the two wake functions. The file's header lists what changed. | `467f3ec158a8bdc3fd4f79d7d885841beacf6ea3` | MIT | header of the file | 2026-10-26, P1-WALK-01 |

- **Path**: the vendored file or directory in this repository.
- **Upstream**: the source repository and the path in it, and which parts were taken when it is not the whole file.
- **Commit**: the full 40-character SHA of the upstream commit the copy matches. It must resolve in the upstream repository.
- **Licence**: the SPDX identifier. It must be on the `[licenses]` list in `deny.toml`.
- **Notice**: where the upstream copyright notice is kept: a header in the vendored file, or a licence file beside it.
- **Synced**: the date of the last sync and the package that made it.

## Adding a piece

Copy it from a single upstream commit and keep the upstream notice. Add its row in the same pull request.

## Sync procedure

1. A monthly step in `nightly-bench.yml` diffs the vendored scheduler against upstream `main`. When upstream has changed, it opens an issue labelled `vendor-sync`.
2. Syncing is then a package of its own, `P<phase>-REPO-SYNC-<nn>`, which does the following:
   1. Diff the upstream file at the recorded commit against upstream `main`, and read the changes to the parts the row names.
   2. Apply the upstream changes that are wanted, keeping the notice.
   3. Run the walker property tests and the benchmark harness.
   4. Record the new upstream SHA and the sync date in this table.
