CREATE TABLE goal_breaks (
    id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    label TEXT NOT NULL,
    start_date TEXT NOT NULL,   -- YYYY-MM-DD
    end_date TEXT NOT NULL,     -- YYYY-MM-DD
    created_at TEXT NOT NULL
);
CREATE INDEX idx_goal_breaks_goal ON goal_breaks(goal_id);
