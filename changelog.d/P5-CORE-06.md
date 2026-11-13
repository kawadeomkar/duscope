### Fixed

- On macOS, `clean` answers `trash_unavailable`, whose hint suggests `--permanent`, only when the item's volume has no Trash, which it now tells by the system's error code instead of the words of its message. Any other refusal to move an item to the Trash is `io_error`: a file that went missing just before the move, a Trash that could not be made, and any error duscope does not know, which were `trash_unavailable` unless their message spoke of permission. The message ends with the error's domain and code, for example `(NSCocoaErrorDomain 513)` for a locked file.
