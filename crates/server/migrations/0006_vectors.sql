-- RAG memory store. Embeddings are kept as little-endian f32 BLOBs in a plain
-- SQLite table; similarity search is a brute-force cosine scan in Rust (see
-- `crate::vector`). This avoids the sqlite-vec C extension / libsqlite3-sys
-- linking and is more than adequate at this scale. sqlite-vec / LanceDB are the
-- documented scale-up path.

CREATE TABLE embeddings (
    id         TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    kind       TEXT NOT NULL,   -- goal | step | message | tip
    source_id  TEXT NOT NULL,   -- id of the row this embedding summarizes
    model      TEXT NOT NULL,   -- embedding model id used
    dim        INTEGER NOT NULL,
    vec        BLOB NOT NULL,   -- dim * 4 bytes, little-endian f32
    text       TEXT NOT NULL,   -- source text (returned by search for RAG)
    created_at TEXT NOT NULL
);

CREATE INDEX idx_embeddings_user ON embeddings(user_id);
CREATE UNIQUE INDEX idx_embeddings_src ON embeddings(user_id, kind, source_id);
