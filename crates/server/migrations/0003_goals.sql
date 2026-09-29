CREATE TABLE goals (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, title TEXT NOT NULL, description TEXT, category TEXT, deadline TEXT, status TEXT NOT NULL DEFAULT 'draft', progress_note TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE INDEX idx_goals_user ON goals(user_id);
CREATE TABLE roadmaps (id TEXT PRIMARY KEY, goal_id TEXT NOT NULL UNIQUE, model TEXT, created_at TEXT NOT NULL);
CREATE TABLE roadmap_steps (id TEXT PRIMARY KEY, roadmap_id TEXT NOT NULL, ord INTEGER NOT NULL, title TEXT NOT NULL, detail TEXT, due_date TEXT, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL);
CREATE INDEX idx_steps_roadmap ON roadmap_steps(roadmap_id);
