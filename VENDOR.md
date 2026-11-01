# Vendored code

Code copied into this repository from another project has one row here, giving where it lives, where it came from, the upstream commit it matches, its licence and where the upstream notice is kept.

| Path | Upstream | Commit | Licence | Notice | Synced |
| --- | --- | --- | --- | --- | --- |
| `crates/duscope-walk/src/scheduler.rs` | [Byron/dua-cli](https://github.com/Byron/dua-cli), `crates/dua-lib/src/lib.rs` (dua-core 5.0.1): the worker pool, which is `PoolShared`, `Pool`, `start_pool`, `worker_loop`, `find_job`, `schedule_jobs` and the two wake functions. The file's header lists what changed. | `467f3ec158a8bdc3fd4f79d7d885841beacf6ea3` | MIT | header of the file | 2026-10-26, P1-WALK-01 |
| `crates/duscope-treemap/src/packs.rs`, the Mocha pack | [catppuccin/catppuccin](https://github.com/catppuccin/catppuccin), `README.md`: the Mocha palette's colours, all twenty the pack uses (roles, classes, cycle and age ramp). | `d09787dd98ca6fba08af5ef2ae94a7e09f17daca` | MIT | header of the file | 2026-10-27, P1-MAP-02 |
| `crates/duscope-treemap/src/packs.rs`, the Tokyo Night pack | [enkia/tokyo-night-vscode-theme](https://github.com/enkia/tokyo-night-vscode-theme), `themes/tokyo-night-color-theme.json`: every colour of the pack but the four in the next row. | `da5546bc4163a02a30d6f3ced90d4ef7dfcb8460` | MIT | header of the file | 2026-10-27, P1-MAP-02 |
| `crates/duscope-treemap/src/packs.rs`, the Tokyo Night pack | [folke/tokyonight.nvim](https://github.com/folke/tokyonight.nvim), `lua/tokyonight/colors/storm.lua` (which `night.lua` extends): `#3b4261` (surface2), `#565f89` (overlay1), `#737aa2` (the other class) and `#ff007c` (the cycle's tenth). | `9578c6fe68af1db513914a2a01bea5585f3027db` | Apache-2.0 | header of the file; the licence text is `LICENSE-APACHE` | 2026-10-27, P1-MAP-02 |
| `crates/duscope-treemap/src/packs.rs`, the Dracula pack | [dracula/dracula-theme](https://github.com/dracula/dracula-theme), `README.md`: the Dracula palette's eleven colours. The darker roles and the muted ones (`#21222c`, `#191a21`, `#565a6e`, `#7e8ab8`, `#bfc7d5`) are the proposal's role table, and `#a4ffff` and `#ffd1e8` were chosen so no class colour equals a drawn role. | `1e04a4b768302fa2de37d09cc7b8087eabcd3ed8` | MIT | header of the file | 2026-10-27, P1-MAP-02 |
| `crates/duscope-treemap/src/packs.rs`, the Gruvbox Dark pack | [morhetz/gruvbox](https://github.com/morhetz/gruvbox), `colors/gruvbox.vim`: the dark palette's colours; crust `#141617` is the proposal's role table. | `040138616bec342d5ea94d4db296f8ddca17007a` | MIT | header of the file | 2026-10-27, P1-MAP-02 |
| `crates/duscope-treemap/src/packs.rs`, the light pack | [Gioni06/terminal.css](https://github.com/Gioni06/terminal.css), `lib/terminal.css`: the background `#fff`, font `#151515`, secondary `#727578`, primary `#1a95e0` and error `#d20962` colours. The pack's other roles and its class, cycle and age colours were chosen to go with them. | `8fad2b4936c209dfab52a386fd4a2757ba4b655a` | MIT | header of the file | 2026-10-27, P1-MAP-02 |
| `crates/duscope-treemap/src/font/Inter-Regular-subset.ttf` | [rsms/inter](https://github.com/rsms/inter), `extras/ttf/Inter-Regular.ttf` of the v4.1 release (`Inter-4.1.zip`): the glyphs of ASCII, Latin-1, the dashes, quotes, `•`, `…` and `€`, cut by `scripts/subset_font.py`, which flattens the composite glyphs and drops the hinting and the layout tables; the name table, with the copyright and licence, is kept. | `e3a3d4c57d5ecc01453a575621882a384c1995a3` | OFL-1.1 | `crates/duscope-treemap/src/font/OFL.txt` | 2026-11-01, P2-MAP-02 |

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
