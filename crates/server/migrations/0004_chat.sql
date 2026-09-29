CREATE TABLE conversations (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, goal_id TEXT, title TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
CREATE INDEX idx_conversations_user ON conversations(user_id);
CREATE TABLE messages (id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, token_cost_cents INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);
CREATE INDEX idx_messages_conversation ON messages(conversation_id);
