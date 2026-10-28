# Golden files

Each file here is the expected output of one command or one painted buffer: the list verbs'
`--json`, `--ndjson`, `--plain` and table output, the scan's `--json`, the treemap's cell
buffers and `duscope --help`. The tests compare through `duscope_testkit::golden`, after
`duscope_testkit::normalize` takes out what differs between runs and machines (the fixture
tree's path, times, volumes, the version, a table footer's duration); a test replaces what only
its output holds, such as the thread count, with `duscope_testkit::replace_values`.

A file whose output differs between the platforms has one copy per OS, `name@macos.snap` and
`name@linux.snap`; one that does not (`help_text_duscope.snap`) has a single copy. No golden
is skipped on either OS.

## After an intended output change

```sh
just golden                 # regenerate this platform's goldens; lists the changed files
just golden treemap         # only the tests whose names contain "treemap"
```

`just golden` runs the tests with `DUSCOPE_TEST_UPDATE_GOLDEN=1`, which makes each golden test
write its file instead of comparing. Review the diff: every changed line should be one the
change meant to make.

Regenerate both platforms' copies in the same pull request. On a Mac, the Linux copies come
from a Linux machine or container running `just golden`; or push, and take them from the
`goldens-<runner>` artifact the CI `test` job uploads when a golden test fails, which holds
that runner's regenerated files.
