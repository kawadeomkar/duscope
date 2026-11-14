### Fixed

- Ctrl-C while a scan is being saved stops the save: duscope exits 130 with `duscope: interrupted`
  and leaves neither the index nor its temporary file; the output already printed stands. Ctrl-C
  was ignored there, and the save, seconds long on a large tree, ran to its end.
- `duscope_scan` cancelled, or past its deadline, while it saves its scan saves nothing; the save
  used to run to its end after the call had been answered.
