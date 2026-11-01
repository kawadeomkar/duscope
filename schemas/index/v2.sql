-- duscope index schema, index_version 2.0: the DDL of the SQLite file each scan writes.
--
-- docs/index-format.md describes every column. A new nullable column is a minor
-- version, which older readers open and ignore; a change of meaning is a major one,
-- which they refuse with the rescan hint. crates/duscope-core/src/index/schema.sql
-- is this file, byte for byte, for the writer to run.
--
-- Entries are numbered in pre-order from 0, the root, in one id space across `dirs`
-- and `files`: a directory's subtree is `id .. last_id` and its own files are
-- `id + 1 .. files_end`. Sizes are bytes, times signed nanoseconds since the Unix
-- epoch (UTC), names and paths the bytes the filesystem returned.

-- The scan: one row per key, every key present (docs/index-format.md lists them).
CREATE TABLE scan_meta (
    key   TEXT NOT NULL PRIMARY KEY,
    value
) WITHOUT ROWID;

-- Every volume the scan met, entered or not.
CREATE TABLE scan_volumes (
    dev               INTEGER NOT NULL,
    mnt_id            INTEGER NOT NULL,
    mount_point       BLOB    NOT NULL,
    fs_type           TEXT    NOT NULL,
    read_only         INTEGER NOT NULL,
    entered           INTEGER NOT NULL,
    reason            TEXT,
    case_sensitive    INTEGER,
    normalization     TEXT    NOT NULL,
    fsevents_uuid     TEXT,
    fsevents_event_id INTEGER,
    os_build          TEXT,
    total_bytes       INTEGER,
    free_bytes        INTEGER,
    options           TEXT
);

-- Directories, with their own sizes and their subtree totals.
CREATE TABLE dirs (
    id                INTEGER PRIMARY KEY,
    last_id           INTEGER NOT NULL,
    files_end         INTEGER NOT NULL,
    parent_id         INTEGER,
    depth             INTEGER NOT NULL,
    name              BLOB    NOT NULL,
    apparent          INTEGER NOT NULL,
    allocated         INTEGER NOT NULL,
    cum_apparent      INTEGER NOT NULL,
    cum_allocated     INTEGER NOT NULL,
    cum_du_compatible INTEGER NOT NULL,
    n_files           INTEGER NOT NULL,
    n_dirs            INTEGER NOT NULL,
    mtime             INTEGER NOT NULL,
    ctime             INTEGER NOT NULL,
    max_mtime         INTEGER NOT NULL,
    dev               INTEGER NOT NULL,
    flags             INTEGER NOT NULL,
    errors            INTEGER,
    atime             INTEGER,
    btime             INTEGER,
    mode              INTEGER,
    uid               INTEGER,
    gid               INTEGER
);

-- Every entry that is not a directory.
CREATE TABLE files (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER NOT NULL,
    name      BLOB    NOT NULL,
    apparent  INTEGER NOT NULL,
    allocated INTEGER NOT NULL,
    charged   INTEGER NOT NULL,
    mtime     INTEGER NOT NULL,
    ctime     INTEGER NOT NULL,
    atime     INTEGER,
    btime     INTEGER,
    mode      INTEGER,
    uid       INTEGER,
    gid       INTEGER,
    nlink     INTEGER NOT NULL,
    kind      TEXT    NOT NULL,
    flags     INTEGER NOT NULL,
    ext       TEXT    NOT NULL
);

-- Inodes with more than one link, met at least once.
CREATE TABLE hardlinks (
    dev      INTEGER NOT NULL,
    ino      INTEGER NOT NULL,
    first_id INTEGER NOT NULL,
    nlink    INTEGER NOT NULL,
    seen     INTEGER NOT NULL,
    PRIMARY KEY (dev, ino)
) WITHOUT ROWID;

-- Clone groups (macOS, with --clones).
CREATE TABLE clones (
    dev      INTEGER NOT NULL,
    clone_id INTEGER NOT NULL,
    first_id INTEGER NOT NULL,
    PRIMARY KEY (dev, clone_id)
) WITHOUT ROWID;

-- Each hard-linked file and clone member, with its group: the lowest id
-- among its links (hardlinks.first_id), and among its clone group's members
-- (clones.first_id), so explain tells links inside a subtree from links out.
CREATE TABLE shared_members (
    id          INTEGER PRIMARY KEY,
    link_first  INTEGER,
    clone_first INTEGER
);

-- Entries a directory listed but could not read.
CREATE TABLE scan_errors (
    dir_id INTEGER NOT NULL,
    name   BLOB    NOT NULL,
    errno  INTEGER NOT NULL,
    PRIMARY KEY (dir_id, name)
) WITHOUT ROWID;

-- The whole scan's files by extension.
CREATE TABLE type_stats (
    ext       TEXT    NOT NULL PRIMARY KEY,
    n         INTEGER NOT NULL,
    allocated INTEGER NOT NULL,
    apparent  INTEGER NOT NULL
);

INSERT INTO scan_meta (key, value) VALUES ('index_version', '2.0');

-- Indexes, created after the bulk insert.
CREATE INDEX dirs_parent_name ON dirs (parent_id, name);
CREATE INDEX dirs_cum_allocated ON dirs (cum_allocated DESC);
CREATE INDEX files_allocated ON files (allocated DESC);
CREATE INDEX files_mtime ON files (mtime);
CREATE INDEX dirs_errors ON dirs (id) WHERE errors IS NOT NULL;
CREATE INDEX dirs_excluded ON dirs (id) WHERE flags & 3072 != 0;
CREATE INDEX files_dataless ON files (id) WHERE flags & 4 != 0;
