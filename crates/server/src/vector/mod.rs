//! Vector store for RAG memory.
//!
//! Embeddings are stored as little-endian `f32` BLOBs in a normal SQLite table
//! (migration `0006_vectors.sql`) and similarity search is a brute-force cosine
//! scan in Rust. This keeps the dependency surface to just SQLite (no sqlite-vec
//! C extension / libsqlite3-sys linking), which is more than adequate at this
//! scale. sqlite-vec / LanceDB are the documented scale-up path.
//!
//! Surface:
//!   `upsert(pool, user_id, kind, source_id, model, text, embedding) -> AppResult<()>`
//!   `search(pool, user_id, embedding, k, kinds) -> AppResult<Vec<VecHit>>`
//!   `struct VecHit { kind, source_id, text, score }`  (score = cosine, higher = closer)

use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{AppError, AppResult};

/// One retrieved memory row, scored by cosine similarity to the query vector
/// (higher `score` means more similar / closer).
#[derive(Debug, Clone, PartialEq)]
pub struct VecHit {
    pub kind: String,
    pub source_id: String,
    pub text: String,
    pub score: f32,
}

/// Encode a vector as little-endian `f32` bytes (`4 * len` bytes).
fn encode(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode little-endian `f32` bytes back into a vector. Trailing bytes that
/// don't form a full `f32` are ignored.
fn decode(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Cosine similarity of two equal-length, non-zero vectors. Returns `None` on a
/// length mismatch, an empty vector, or a zero-magnitude vector.
fn cosine(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.is_empty() || a.len() != b.len() {
        return None;
    }
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return None;
    }
    Some(dot / (na.sqrt() * nb.sqrt()))
}

/// Insert or replace the embedding for `(user_id, kind, source_id)`.
pub async fn upsert(
    pool: &SqlitePool,
    user_id: &str,
    kind: &str,
    source_id: &str,
    model: &str,
    text: &str,
    embedding: &[f32],
) -> AppResult<()> {
    let id = uuid::Uuid::new_v4().to_string();
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))?;
    let dim = embedding.len() as i64;
    let blob = encode(embedding);

    sqlx::query(
        "INSERT INTO embeddings (id, user_id, kind, source_id, model, dim, vec, text, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(user_id, kind, source_id) DO UPDATE SET \
             vec = excluded.vec, \
             text = excluded.text, \
             dim = excluded.dim, \
             model = excluded.model, \
             created_at = excluded.created_at",
    )
    .bind(&id)
    .bind(user_id)
    .bind(kind)
    .bind(source_id)
    .bind(model)
    .bind(dim)
    .bind(blob)
    .bind(text)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(())
}

/// Return the top-`k` of `user_id`'s embeddings by cosine similarity to
/// `embedding`, most similar first. When `kinds` is non-empty, only those kinds
/// are considered. Rows whose stored dimension differs from the query length,
/// and zero-magnitude vectors, are skipped.
pub async fn search(
    pool: &SqlitePool,
    user_id: &str,
    embedding: &[f32],
    k: usize,
    kinds: &[&str],
) -> AppResult<Vec<VecHit>> {
    if k == 0 || embedding.is_empty() {
        return Ok(Vec::new());
    }

    let mut sql =
        String::from("SELECT kind, source_id, text, vec, dim FROM embeddings WHERE user_id = ?");
    if !kinds.is_empty() {
        sql.push_str(" AND kind IN (");
        for (i, _) in kinds.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push(')');
    }

    let mut q = sqlx::query_as::<_, (String, String, String, Vec<u8>, i64)>(&sql).bind(user_id);
    for kind in kinds {
        q = q.bind(*kind);
    }

    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let mut hits: Vec<VecHit> = rows
        .into_iter()
        .filter_map(|(kind, source_id, text, vec, dim)| {
            if dim as usize != embedding.len() {
                return None;
            }
            let stored = decode(&vec);
            let score = cosine(embedding, &stored)?;
            Some(VecHit {
                kind,
                source_id,
                text,
                score,
            })
        })
        .collect();

    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hits.truncate(k);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        (dir, pool)
    }

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![0.0f32, 1.5, -2.25, 1e10, -0.0001];
        let bytes = encode(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        assert_eq!(decode(&bytes), v);
    }

    #[test]
    fn cosine_ranks_similarity() {
        let q = vec![1.0f32, 0.0, 0.0];
        let same = cosine(&q, &vec![2.0, 0.0, 0.0]).unwrap();
        let ortho = cosine(&q, &vec![0.0, 1.0, 0.0]).unwrap();
        let opposite = cosine(&q, &vec![-1.0, 0.0, 0.0]).unwrap();
        assert!((same - 1.0).abs() < 1e-6);
        assert!(ortho.abs() < 1e-6);
        assert!((opposite + 1.0).abs() < 1e-6);
        assert!(same > ortho && ortho > opposite);
    }

    #[test]
    fn cosine_guards() {
        assert_eq!(cosine(&[], &[]), None);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), None);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), None);
    }

    #[tokio::test]
    async fn upsert_search_ranks_and_filters() {
        let (_dir, pool) = test_pool().await;

        upsert(&pool, "u1", "goal", "g1", "m", "run a 5k", &[1.0, 0.0, 0.0])
            .await
            .unwrap();
        upsert(
            &pool,
            "u1",
            "message",
            "m1",
            "m",
            "off topic",
            &[0.0, 1.0, 0.0],
        )
        .await
        .unwrap();
        upsert(
            &pool,
            "u1",
            "goal",
            "g2",
            "m",
            "jog daily",
            &[0.9, 0.1, 0.0],
        )
        .await
        .unwrap();
        // Another user's row must never surface.
        upsert(
            &pool,
            "u2",
            "goal",
            "gX",
            "m",
            "someone else",
            &[1.0, 0.0, 0.0],
        )
        .await
        .unwrap();

        let hits = search(&pool, "u1", &[1.0, 0.0, 0.0], 10, &[])
            .await
            .unwrap();
        let order: Vec<String> = hits.iter().map(|h| h.source_id.clone()).collect();
        assert_eq!(order, vec!["g1", "g2", "m1"]); // exact, close, orthogonal

        // kinds filter.
        let goals = search(&pool, "u1", &[1.0, 0.0, 0.0], 10, &["goal"])
            .await
            .unwrap();
        assert_eq!(goals.len(), 2);
        assert!(goals.iter().all(|h| h.kind == "goal"));

        // k truncation.
        let top1 = search(&pool, "u1", &[1.0, 0.0, 0.0], 1, &[]).await.unwrap();
        let top1_ids: Vec<String> = top1.iter().map(|h| h.source_id.clone()).collect();
        assert_eq!(top1_ids, vec!["g1"]);
    }

    #[tokio::test]
    async fn upsert_replaces_and_skips_dim_mismatch() {
        let (_dir, pool) = test_pool().await;

        upsert(&pool, "u1", "goal", "g1", "m", "old", &[1.0, 0.0])
            .await
            .unwrap();
        upsert(&pool, "u1", "goal", "g1", "m", "new", &[0.0, 1.0])
            .await
            .unwrap();

        // Only one row for the conflict key, with the updated text.
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM embeddings WHERE user_id='u1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);

        // Query dim (3) mismatches stored dim (2) → row skipped, no panic.
        let hits = search(&pool, "u1", &[0.0, 1.0, 0.0], 5, &[]).await.unwrap();
        assert!(hits.is_empty());

        let hits = search(&pool, "u1", &[0.0, 1.0], 5, &[]).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits.first().map(|h| h.text.as_str()), Some("new"));
    }
}
