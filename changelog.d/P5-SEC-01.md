### Security

- `export -o FILE` writes the export to `.FILE.<pid>.part` beside the file first, a name anyone
  who may write that directory can guess. It followed a symbolic link put there, so in a
  directory others share (`/tmp`) a planted link had the export overwrite the file it led to,
  since v0.2.0. The temporary file is now made only where nothing is at its name; a link there
  fails the export, whose error names the temporary file, and is removed.
