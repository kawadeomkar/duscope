### Added

- `duscope mcp` runs tool calls one at a time, in the order they arrive, while it
  keeps answering `server/discover`, `tools/list` and `ping` at once. A client
  can cancel a call with `notifications/cancelled`: a waiting call never runs, a
  running `duscope_scan` stops its walk, and the call gets no response.
- `duscope_scan` sends `notifications/progress` every two seconds with the
  entries counted so far, when the request names a `progressToken`.
- Tool calls have deadlines: 10 minutes for `duscope_scan` and 30 seconds for
  the other tools (`duscope_query`'s statement keeps the sandbox's 5 seconds and
  its `sql_timeout`). A call past its deadline returns an error result with the
  `deadline_exceeded` code, and a scan past it stops.
