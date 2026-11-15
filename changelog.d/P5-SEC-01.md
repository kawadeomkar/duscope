### Security

- `export -o FILE` writes the export to `.FILE.<pid>.part` beside the file first, a name anyone
  who may write that directory can guess. It followed a symbolic link put there, so in a
  directory others share (`/tmp`) a planted link had the export overwrite the file it led to,
  since v0.2.0. The temporary file is now made only where nothing is at its name; a link there
  fails the export, whose error names the temporary file, and is removed.
- `agent --skill` followed symbolic links below the folder it writes the skill to, since v0.2.0.
  A `.claude` or `.agents` folder, or a folder below either, that is a link had the skill's files
  made where it led; a link at a file's temporary name (`SKILL.md.tmp-<pid>`, guessable) had the
  skill's text overwrite the file it led to, and the command exited 0. A folder someone else
  prepared, such as a cloned repository, can hold such links. It now refuses a link at each
  folder below the one given and at each skill file (`refused_path`) before writing anything,
  makes each missing folder without following a link, and makes each temporary file only where
  nothing is at its name.
