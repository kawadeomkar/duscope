### Changed

- The viewer opens on a large saved scan in less than half the time: it reads the scan once to
  classify its entries, which also gives each folder's colour on the map and the legend's rows,
  where it read it three times. On a million entries (T2) the first frame comes after about 0.5 s
  instead of 1.2 s.
