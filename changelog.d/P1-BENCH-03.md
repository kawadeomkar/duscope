### Changed

- The Linux release builds (statically linked with musl, the archives and
  `.deb` packages) allocate with mimalloc: a warm scan of a million entries
  takes 0.4 s where musl's own allocator took 3.0 s. macOS keeps the
  system allocator, which mimalloc did not beat.
