### Security

- rustix 1.1.6, for RUSTSEC-2026-0486: rustix 1.1.5's extended-attribute functions were unsound on a zero-length buffer. duscope calls none of them, so nothing changes in what it does; the binary no longer carries the release the advisory names.
