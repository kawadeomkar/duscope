### Changed

- `stale`'s help, the manual page and the briefing `duscope agent` prints (and the skill
  `agent --skill` writes) say that `stale` lists a file only when its change time (ctime) is
  older than `--older-than` as well as its modification time, so a file restored, or copied
  with its times kept, is not listed; for the modification time alone they name
  `duscope top --files --older-than AGE`. The briefing answers "large files not modified in
  months" with `top --files --older-than`, and keeps `stale` for files neither modified nor
  changed.
