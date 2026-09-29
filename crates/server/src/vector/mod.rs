//! Vector store for RAG memory.
//!
//! STUB — implemented by the `llm` feature agent. Approach (per
//! docs/ARCHITECTURE.md): store embeddings as f32 BLOBs in a normal SQLite
//! table (migration `0006_vectors.sql`) and do brute-force cosine similarity in
//! Rust. This keeps the dependency surface to just SQLite (no sqlite-vec C
//! extension / libsqlite3-sys linking), which is more than adequate at this
//! scale. sqlite-vec / LanceDB are the documented scale-up path.
//!
//! Expected surface:
//!   pub async fn upsert(pool, user_id, kind, source_id, text, embedding) -> AppResult<()>
//!   pub async fn search(pool, user_id, embedding, k, kinds) -> AppResult<Vec<VecHit>>
//!   pub struct VecHit { kind, source_id, text, distance }
