//! Circles: privacy-preserving goal-similarity matching.
//!
//! Users are matched to each other by the semantic similarity of their goals,
//! reusing the vector store (`kind = "goal"` embeddings) and the brute-force
//! cosine scan in [`crate::vector`]. The product promises that goals stay
//! private, so this module NEVER exposes another user's goal text, title,
//! description, `source_id`, or `user_id` to the caller. Only aggregate/derived
//! information leaves the server:
//!   - the goal *category* of matching other-user goals,
//!   - a *count* of distinct OTHER users with a similar goal,
//!   - the *average* cosine similarity of the matches.
//!
//! Routes (all under `RequireAuth`):
//!   - `GET /api/circles/suggestions`  suggested circles grouped by category
//!   - `GET /api/goals/{id}/similar`   similar-user count for one owned goal
//!
//! Owns no migration — it reuses the `embeddings` and `goals` tables.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Minimum cosine similarity for two goals to count as "similar". Cosine ranges
/// [-1, 1]; 0.6 is a deliberately conservative bar for a real match.
const SIMILARITY_THRESHOLD: f32 = 0.6;

/// How many nearest other-user goals to consider per caller goal / per query.
const NEIGHBORS: usize = 20;

/// Category bucket used when a matching goal has no category set. Kept generic so
/// it never reveals anything about the other user's goal.
const UNCATEGORIZED: &str = "uncategorized";

// --- DTOs ---------------------------------------------------------------------

/// A suggested circle: an aggregate over OTHER users' goals in one category that
/// are similar to one of the caller's goals. Contains no other-user identity or
/// goal text — only derived counts/scores and a pointer back to the *caller's*
/// own goal that produced the match.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CircleSuggestion {
    /// Category of the matching other-user goals (server-side lookup only).
    pub category: String,
    /// Distinct count of OTHER users with a similar goal in this category.
    pub member_count: i64,
    /// Mean cosine similarity of the matches in this category.
    pub avg_score: f32,
    /// The caller's own goal that produced the most matches in this category.
    pub based_on_goal_id: String,
    /// Title of that caller-owned goal.
    pub based_on_goal_title: String,
}

#[derive(Debug, Serialize)]
struct SuggestionsResponse {
    circles: Vec<CircleSuggestion>,
}

/// Similar-user summary for a single owned goal. No other-user identity or text.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct SimilarResponse {
    /// Distinct count of OTHER users whose goal matches this one at/above the
    /// similarity threshold.
    similar_user_count: i64,
    /// Mean cosine similarity of those matches (0.0 when there are none).
    avg_score: f32,
}

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/circles/suggestions", get(suggestions))
        .route("/api/goals/{id}/similar", get(goal_similar))
}

// --- Internals ----------------------------------------------------------------

/// A caller-owned goal that has a stored embedding.
struct CallerGoal {
    id: String,
    title: String,
    embedding: Vec<f32>,
}

/// Per-category running aggregate while scanning matches.
#[derive(Default)]
struct CategoryAgg {
    /// Distinct OTHER user ids seen in this category (never returned).
    user_ids: HashSet<String>,
    score_sum: f64,
    score_n: usize,
    /// caller goal id -> (match count, caller goal title).
    per_caller_goal: HashMap<String, (usize, String)>,
}

/// Load the caller's goals that have a stored `goal` embedding, decoding each
/// vector. Rows whose decoded length disagrees with the stored `dim` are skipped
/// (defensive: never trust a malformed blob).
async fn caller_goals(pool: &SqlitePool, user_id: &str) -> AppResult<Vec<CallerGoal>> {
    let rows = sqlx::query_as::<_, (String, String, Vec<u8>, i64)>(
        "SELECT g.id, g.title, e.vec, e.dim \
         FROM goals g \
         JOIN embeddings e \
           ON e.kind = 'goal' AND e.source_id = g.id AND e.user_id = g.user_id \
         WHERE g.user_id = ?",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let out = rows
        .into_iter()
        .filter_map(|(id, title, vec, dim)| {
            let embedding = crate::vector::decode(&vec);
            if embedding.len() != dim as usize || embedding.is_empty() {
                return None;
            }
            Some(CallerGoal {
                id,
                title,
                embedding,
            })
        })
        .collect();
    Ok(out)
}

/// Server-side lookup of a matched goal's category, memoized. A goal with no
/// category (or somehow missing) is bucketed as [`UNCATEGORIZED`].
async fn category_for(
    pool: &SqlitePool,
    cache: &mut HashMap<String, String>,
    source_id: &str,
) -> AppResult<String> {
    if let Some(cat) = cache.get(source_id) {
        return Ok(cat.clone());
    }
    let row: Option<(Option<String>,)> = sqlx::query_as("SELECT category FROM goals WHERE id = ?")
        .bind(source_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let category = row
        .and_then(|(c,)| c)
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| UNCATEGORIZED.to_string());
    cache.insert(source_id.to_string(), category.clone());
    Ok(category)
}

/// Compute suggested circles for `user_id`. Returns an empty vector when the
/// caller has no embedded goals or no sufficiently-similar other users — that is
/// the honest normal case early on, not an error.
async fn compute_suggestions(pool: &SqlitePool, user_id: &str) -> AppResult<Vec<CircleSuggestion>> {
    let goals = caller_goals(pool, user_id).await?;

    let mut by_category: HashMap<String, CategoryAgg> = HashMap::new();
    let mut cat_cache: HashMap<String, String> = HashMap::new();

    for goal in &goals {
        let hits = crate::vector::search_across_users(
            pool,
            user_id,
            &goal.embedding,
            NEIGHBORS,
            &["goal"],
        )
        .await?;
        for hit in hits {
            if hit.score < SIMILARITY_THRESHOLD {
                continue;
            }
            let category = category_for(pool, &mut cat_cache, &hit.source_id).await?;
            let agg = by_category.entry(category).or_default();
            agg.user_ids.insert(hit.user_id);
            agg.score_sum += hit.score as f64;
            agg.score_n += 1;
            let entry = agg
                .per_caller_goal
                .entry(goal.id.clone())
                .or_insert_with(|| (0, goal.title.clone()));
            entry.0 += 1;
        }
    }

    let mut out: Vec<CircleSuggestion> = Vec::new();
    for (category, agg) in by_category {
        let member_count = agg.user_ids.len() as i64;
        if member_count < 1 {
            continue;
        }
        let avg_score = if agg.score_n == 0 {
            0.0
        } else {
            (agg.score_sum / agg.score_n as f64) as f32
        };
        // The caller's goal that produced the most matches in this category.
        let best = agg
            .per_caller_goal
            .iter()
            .max_by_key(|(_, (count, _))| *count);
        let (based_on_goal_id, based_on_goal_title) = match best {
            Some((gid, (_, title))) => (gid.clone(), title.clone()),
            None => continue,
        };
        out.push(CircleSuggestion {
            category,
            member_count,
            avg_score,
            based_on_goal_id,
            based_on_goal_title,
        });
    }

    // Sort by member_count desc, then avg_score desc.
    out.sort_by(|a, b| {
        b.member_count.cmp(&a.member_count).then(
            b.avg_score
                .partial_cmp(&a.avg_score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    Ok(out)
}

/// Compute the similar-user summary for one goal owned by `user_id`. Enforces
/// ownership (NotFound / Forbidden), and returns a zeroed summary when the goal
/// has no embedding yet (embeddings require the OpenRouter key to have been set
/// when the goal was created).
async fn goal_similar_summary(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: &str,
) -> AppResult<SimilarResponse> {
    // Ownership check — never operate on someone else's goal.
    let owner: Option<(String,)> = sqlx::query_as("SELECT user_id FROM goals WHERE id = ?")
        .bind(goal_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    match owner {
        None => return Err(AppError::NotFound),
        Some((owner_id,)) if owner_id != user_id => return Err(AppError::Forbidden),
        Some(_) => {}
    }

    // Fetch this goal's own embedding (if any).
    let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
        "SELECT vec, dim FROM embeddings \
         WHERE user_id = ? AND kind = 'goal' AND source_id = ?",
    )
    .bind(user_id)
    .bind(goal_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let Some((vec, dim)) = row else {
        return Ok(SimilarResponse {
            similar_user_count: 0,
            avg_score: 0.0,
        });
    };
    let embedding = crate::vector::decode(&vec);
    if embedding.len() != dim as usize || embedding.is_empty() {
        return Ok(SimilarResponse {
            similar_user_count: 0,
            avg_score: 0.0,
        });
    }

    let hits =
        crate::vector::search_across_users(pool, user_id, &embedding, NEIGHBORS, &["goal"]).await?;
    let (similar_user_count, avg_score) = summarize(&hits, SIMILARITY_THRESHOLD);
    Ok(SimilarResponse {
        similar_user_count,
        avg_score,
    })
}

/// Reduce cross-user hits to (distinct other-user count, mean score) over the
/// matches at/above `threshold`. Pure — no I/O, easily unit-testable.
fn summarize(hits: &[crate::vector::CrossHit], threshold: f32) -> (i64, f32) {
    let mut users: HashSet<&str> = HashSet::new();
    let mut sum = 0f64;
    let mut n = 0usize;
    for hit in hits {
        if hit.score >= threshold {
            users.insert(hit.user_id.as_str());
            sum += hit.score as f64;
            n += 1;
        }
    }
    let avg = if n == 0 { 0.0 } else { (sum / n as f64) as f32 };
    (users.len() as i64, avg)
}

// --- Handlers -----------------------------------------------------------------

/// `GET /api/circles/suggestions` — suggested circles grouped by category.
///
/// Response shape: `{ "circles": [ { category, member_count, avg_score,
/// based_on_goal_id, based_on_goal_title } ... ] }`. Empty array when there is
/// nothing to suggest yet.
async fn suggestions(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<SuggestionsResponse>> {
    let circles = compute_suggestions(&state.db, &user_id).await?;
    Ok(Json(SuggestionsResponse { circles }))
}

/// `GET /api/goals/{id}/similar` — similar-user summary for one owned goal.
///
/// Response shape: `{ "similar_user_count": <i64>, "avg_score": <f32> }`.
async fn goal_similar(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
) -> AppResult<Json<SimilarResponse>> {
    Ok(Json(goal_similar_summary(&state.db, &user_id, &id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let path = db_path.to_str().expect("utf8 path");
        let pool = crate::db::init_pool(path).await.expect("init pool");
        (dir, pool)
    }

    /// Insert a bare goal row (only the columns we need for matching).
    async fn insert_goal(
        pool: &SqlitePool,
        id: &str,
        user_id: &str,
        title: &str,
        category: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO goals \
                 (id, user_id, title, description, category, deadline, status, progress_note, \
                  created_at, updated_at) \
             VALUES (?, ?, ?, NULL, ?, NULL, 'active', NULL, \
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(user_id)
        .bind(title)
        .bind(category)
        .execute(pool)
        .await
        .expect("insert goal");
    }

    /// Insert a goal embedding via the real vector-store upsert path.
    async fn insert_goal_embedding(
        pool: &SqlitePool,
        user_id: &str,
        goal_id: &str,
        text: &str,
        vec: &[f32],
    ) {
        crate::vector::upsert(pool, user_id, "goal", goal_id, "test-model", text, vec)
            .await
            .expect("upsert embedding");
    }

    #[test]
    fn summarize_counts_distinct_users_above_threshold() {
        let hits = vec![
            crate::vector::CrossHit {
                user_id: "b".into(),
                source_id: "gb".into(),
                kind: "goal".into(),
                score: 0.9,
            },
            // Same user, second matching goal -> counted once for the user,
            // but both scores feed the mean.
            crate::vector::CrossHit {
                user_id: "b".into(),
                source_id: "gb2".into(),
                kind: "goal".into(),
                score: 0.7,
            },
            crate::vector::CrossHit {
                user_id: "c".into(),
                source_id: "gc".into(),
                kind: "goal".into(),
                score: 0.8,
            },
            // Below threshold -> ignored entirely.
            crate::vector::CrossHit {
                user_id: "d".into(),
                source_id: "gd".into(),
                kind: "goal".into(),
                score: 0.1,
            },
        ];
        let (count, avg) = summarize(&hits, SIMILARITY_THRESHOLD);
        assert_eq!(count, 2, "distinct users b and c");
        assert!((avg - 0.8).abs() < 1e-6, "mean of 0.9, 0.7, 0.8 = 0.8");
    }

    #[test]
    fn summarize_empty_is_zero() {
        let (count, avg) = summarize(&[], SIMILARITY_THRESHOLD);
        assert_eq!(count, 0);
        assert_eq!(avg, 0.0);
    }

    #[tokio::test]
    async fn suggestions_finds_similar_user_without_leaking() {
        let (_dir, pool) = test_pool().await;

        // Caller A: one fitness goal.
        insert_goal(&pool, "gA1", "userA", "Run a 5k", Some("fitness")).await;
        insert_goal_embedding(&pool, "userA", "gA1", "run a 5k", &[1.0, 0.0, 0.0]).await;

        // User B: a near-identical fitness goal (cosine ~1) + an unrelated goal.
        insert_goal(
            &pool,
            "gB1",
            "userB",
            "Train for a 5k race",
            Some("fitness"),
        )
        .await;
        insert_goal_embedding(&pool, "userB", "gB1", "train for a 5k", &[0.98, 0.02, 0.0]).await;
        insert_goal(&pool, "gB2", "userB", "Bake sourdough", Some("cooking")).await;
        insert_goal_embedding(&pool, "userB", "gB2", "bake bread", &[0.0, 1.0, 0.0]).await;

        let circles = compute_suggestions(&pool, "userA").await.expect("suggest");
        let first = circles.first().expect("at least one circle");

        assert_eq!(first.category, "fitness");
        assert!(first.member_count >= 1, "one similar other user");
        assert!(first.avg_score >= SIMILARITY_THRESHOLD);
        assert_eq!(first.based_on_goal_id, "gA1");
        assert_eq!(first.based_on_goal_title, "Run a 5k");

        // The orthogonal cooking goal is below threshold -> no cooking circle.
        assert!(circles.iter().all(|c| c.category != "cooking"));

        // Privacy: the serialized response must not leak B's identity or goal
        // text. (The struct simply has no field for them; assert the shape too.)
        let json = serde_json::to_string(&SuggestionsResponse { circles }).expect("serialize");
        assert!(!json.contains("userB"), "must not leak other user id");
        assert!(!json.contains("gB1"), "must not leak other goal source_id");
        assert!(
            !json.contains("Train for a 5k race"),
            "must not leak other goal title"
        );
        assert!(
            !json.contains("train for a 5k"),
            "must not leak other goal text"
        );
    }

    #[tokio::test]
    async fn suggestions_empty_when_no_other_users() {
        let (_dir, pool) = test_pool().await;

        insert_goal(&pool, "gA1", "userA", "Run a 5k", Some("fitness")).await;
        insert_goal_embedding(&pool, "userA", "gA1", "run a 5k", &[1.0, 0.0, 0.0]).await;

        let circles = compute_suggestions(&pool, "userA").await.expect("suggest");
        assert!(circles.is_empty(), "no other users -> no suggestions");
    }

    #[tokio::test]
    async fn suggestions_empty_when_caller_has_no_embedded_goals() {
        let (_dir, pool) = test_pool().await;

        // Caller has a goal but NO embedding (e.g. OpenRouter key was unset).
        insert_goal(&pool, "gA1", "userA", "Run a 5k", Some("fitness")).await;
        // Another user has an embedded goal, but the caller can't match without one.
        insert_goal(&pool, "gB1", "userB", "Run a 5k too", Some("fitness")).await;
        insert_goal_embedding(&pool, "userB", "gB1", "run", &[1.0, 0.0, 0.0]).await;

        let circles = compute_suggestions(&pool, "userA").await.expect("suggest");
        assert!(circles.is_empty());
    }

    #[tokio::test]
    async fn goal_similar_counts_and_is_zero_without_embedding() {
        let (_dir, pool) = test_pool().await;

        insert_goal(&pool, "gA1", "userA", "Run a 5k", Some("fitness")).await;
        insert_goal_embedding(&pool, "userA", "gA1", "run a 5k", &[1.0, 0.0, 0.0]).await;
        // A goal without an embedding.
        insert_goal(&pool, "gA2", "userA", "Learn piano", Some("music")).await;

        // User B matches A1.
        insert_goal(&pool, "gB1", "userB", "5k training", Some("fitness")).await;
        insert_goal_embedding(&pool, "userB", "gB1", "5k", &[0.97, 0.03, 0.0]).await;

        let summary = goal_similar_summary(&pool, "userA", "gA1")
            .await
            .expect("similar");
        assert_eq!(summary.similar_user_count, 1);
        assert!(summary.avg_score >= SIMILARITY_THRESHOLD);

        // No embedding -> honest zero, not an error.
        let none = goal_similar_summary(&pool, "userA", "gA2")
            .await
            .expect("similar");
        assert_eq!(none.similar_user_count, 0);
        assert_eq!(none.avg_score, 0.0);
    }

    #[tokio::test]
    async fn goal_similar_enforces_ownership() {
        let (_dir, pool) = test_pool().await;
        insert_goal(&pool, "gA1", "userA", "Run a 5k", Some("fitness")).await;

        let forbidden = goal_similar_summary(&pool, "intruder", "gA1")
            .await
            .expect_err("forbidden");
        assert!(matches!(forbidden, AppError::Forbidden));

        let missing = goal_similar_summary(&pool, "userA", "ghost")
            .await
            .expect_err("not found");
        assert!(matches!(missing, AppError::NotFound));
    }
}
