# Manual checks

Some acceptance checks need a program that a test cannot drive: a graphical
application, or one that runs only on a platform CI does not cover. Each is
done by hand on the version named here, and this page records the result. A
change to the code a check covers, or a new release of the program, makes the
row due again.

| Check | Program and version | Platform | Package | Last done | Result |
| --- | --- | --- | --- | --- | --- |
| GrandPerspective opens `export --format gpscan` | GrandPerspective 3.8.1 | macOS | P2-CLI-02 | pending | due: the author's check |

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
   and each link of a hard-linked file appears.
5. Record the GrandPerspective version (*GrandPerspective › About*), the date
   and the result in the table.

ncdu needs no row: `p2_cli_02_ncdu_reads_its_export_back` has `ncdu -f` read
the JSON export and write it out again, and CI installs ncdu on its Linux
runners for that test.
