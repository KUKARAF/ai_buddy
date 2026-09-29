CREATE TABLE check_ins (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    goal_id TEXT,
    note TEXT,
    mood TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_checkins_user ON check_ins(user_id, created_at);
CREATE INDEX idx_checkins_goal ON check_ins(goal_id);
