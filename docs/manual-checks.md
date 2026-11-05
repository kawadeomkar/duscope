# Manual checks

Some acceptance checks need a program that a test cannot drive: a graphical
application, or one that runs only on a platform CI does not cover. Each is
done by hand on the version named here, and this page records the result. A
change to the code a check covers, or a new release of the program, makes the
row due again.

| Check | Program and version | Platform | Package | Last done | Result |
| --- | --- | --- | --- | --- | --- |
| GrandPerspective opens `export --format gpscan` | GrandPerspective 3.8.1 | macOS | P2-CLI-02 | 2026-11-04 | passes, both measures (below) |
| Two browsers draw `render`'s SVG alike | Safari and Firefox, or Chrome | macOS | P2-MAP-01 | 2026-11-04 | passes in Safari 27.2's WebKit and Chrome 156 (below) |

## GrandPerspective opens a `.gpscan`

`export --format gpscan` writes GrandPerspective's scan file (format 7, gzip
XML), which the application opens with *File › Open Scan Data…*.

1. `duscope scan target/fixtures.noindex/tree/home --save` (after `just
   fixtures`), then `duscope export target/fixtures.noindex/tree/home --format
   gpscan -o home.gpscan`.
2. `open -a GrandPerspective home.gpscan`, or *File › Open Scan Data…* in the
   application.
3. Expected: the window shows the tree's map with no error; *Window › Show
   Info* gives the volume and the scan time of the saved scan; the largest
   files are where duscope's `top --files` puts them; the hard-linked files of
   the tree appear once (physical sizes).
4. Repeat with `--size apparent`: the file opens with the logical size measure,
   and the hard-linked files still appear once.
5. Record the GrandPerspective version (*GrandPerspective › About*), the date
   and the result in the table.

**Last result** (2026-11-04, GrandPerspective 3.8.1 on macOS 26): both files
open with no error and draw the tree's map; *Show Info* gives Macintosh HD,
the saved scan's time, the volume's size and free space, and *Logical* for
the `--size apparent` file; Movies' files are the largest tiles, where `top
--files` puts them. GrandPerspective counts 30,075 files of the tree's
43,137: the export leaves out the 13,061 empty files, which GrandPerspective
drops too, and the later link of the hard-linked file, so that file counts
once. Its physical total, 220 MB, is duscope's allocated total; its logical
total, 137.6 MiB, is duscope's apparent total less the directories' own
2,160,736 bytes, which APFS gives directories and the format has no place
for.

## Two browsers draw `render`'s SVG alike

`render` writes the treemap as SVG: tiles shaded by two linear gradients
each, labels in the system's sans-serif, and crisp edges.

1. `duscope render target/fixtures.noindex/tree/home -o home.svg` (after a
   saved scan of the tree, as above), and the same with `--layout gp
   --color-by folder -o home-gp.svg`.
2. Open each in two browsers (Safari and Firefox, or Chrome) side by side.
3. Expected: the same tiles in the same places and colours; each shaded
   from light at its top-left corner to dark along its bottom and right
   edges, with no seam along the diagonal; the labels inside their tiles;
   the legend and the totals under the map.
4. Record the browsers' versions, the date and the result in the table.

**Last result** (2026-11-04): Safari 27.2's WebKit (a `WKWebView` snapshot)
and Chrome 156 (headless) draw both files alike, at 1× and 2×: the same
tiles in the same places and colours, each shaded from light at its
top-left corner to dark along its bottom and right edges with no seam on the
diagonal, the labels inside their tiles, and the legend and totals under the
map.

ncdu needs no row: `p2_cli_02_ncdu_reads_its_export_back` has `ncdu -f` read
the JSON export and write it out again, and CI installs ncdu on its Linux
runners for that test.
