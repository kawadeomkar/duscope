### Changed

- On macOS the scan's worker threads ask for the user-initiated QoS class, so a scan started from a context that lowers it, such as a background job or a command another app runs, is no longer held to the efficiency cores, where a scan can take three times as long.
