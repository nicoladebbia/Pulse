-- Migration 012: Chat feedback (thumbs up/down) for RAG quality observability

CREATE TABLE IF NOT EXISTS chat_feedback (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id  TEXT NOT NULL,
    thread_id   TEXT NOT NULL,
    rating      TEXT NOT NULL CHECK(rating IN ('up', 'down')),
    reason      TEXT,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_feedback_message_id ON chat_feedback(message_id);
