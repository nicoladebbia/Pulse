-- Briefings: one per day
CREATE TABLE IF NOT EXISTS briefings (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL UNIQUE,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    story_count     INTEGER NOT NULL DEFAULT 0,
    hero_story_id   INTEGER,
    ai_count        INTEGER NOT NULL DEFAULT 0,
    miami_count     INTEGER NOT NULL DEFAULT 0,
    italy_count     INTEGER NOT NULL DEFAULT 0,
    tech_count      INTEGER NOT NULL DEFAULT 0,
    fetch_duration_ms INTEGER,
    status          TEXT NOT NULL DEFAULT 'complete'
);

-- Stories: the core entity
CREATE TABLE IF NOT EXISTS stories (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    briefing_id     INTEGER NOT NULL REFERENCES briefings(id),
    sector          TEXT NOT NULL CHECK(sector IN ('ai', 'miami', 'italy', 'tech')),
    original_title  TEXT NOT NULL,
    original_url    TEXT NOT NULL,
    original_language TEXT NOT NULL DEFAULT 'en',
    content_snippet TEXT,
    source_name     TEXT NOT NULL,
    published_at    TEXT,
    headline        TEXT NOT NULL,
    summary         TEXT NOT NULL,
    key_facts       TEXT NOT NULL,
    why_it_matters  TEXT NOT NULL,
    what_to_watch   TEXT NOT NULL,
    importance_score INTEGER NOT NULL DEFAULT 5,
    relevance_score  INTEGER,
    relevance_reason TEXT,
    is_hero         INTEGER NOT NULL DEFAULT 0,
    display_order   INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    url_hash        TEXT NOT NULL,
    title_hash      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_stories_briefing ON stories(briefing_id);
CREATE INDEX IF NOT EXISTS idx_stories_sector ON stories(sector);
CREATE INDEX IF NOT EXISTS idx_stories_date ON stories(created_at);

-- Story sources: multiple sources can report the same story
CREATE TABLE IF NOT EXISTS story_sources (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    story_id        INTEGER NOT NULL REFERENCES stories(id),
    source_name     TEXT NOT NULL,
    source_url      TEXT NOT NULL,
    article_url     TEXT NOT NULL,
    is_primary      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_story_sources_story ON story_sources(story_id);

-- Cross-sector connections
CREATE TABLE IF NOT EXISTS cross_connections (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    briefing_id     INTEGER NOT NULL REFERENCES briefings(id),
    story_id_a      INTEGER NOT NULL REFERENCES stories(id),
    story_id_b      INTEGER NOT NULL REFERENCES stories(id),
    connection_text TEXT NOT NULL,
    insight_text    TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_connections_briefing ON cross_connections(briefing_id);

-- Trend threads
CREATE TABLE IF NOT EXISTS trend_threads (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    title           TEXT NOT NULL,
    sector          TEXT NOT NULL,
    trajectory      TEXT NOT NULL DEFAULT 'emerging',
    first_seen      TEXT NOT NULL,
    last_seen       TEXT NOT NULL,
    story_count     INTEGER NOT NULL DEFAULT 1,
    is_active       INTEGER NOT NULL DEFAULT 1
);

CREATE INDEX IF NOT EXISTS idx_trends_active ON trend_threads(is_active, last_seen);

-- Trend points
CREATE TABLE IF NOT EXISTS trend_points (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    thread_id       INTEGER NOT NULL REFERENCES trend_threads(id),
    story_id        INTEGER NOT NULL REFERENCES stories(id),
    date            TEXT NOT NULL,
    significance    INTEGER NOT NULL DEFAULT 5
);

CREATE INDEX IF NOT EXISTS idx_trend_points_thread ON trend_points(thread_id, date);

-- Feed configurations
CREATE TABLE IF NOT EXISTS feed_configs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL,
    feed_type       TEXT NOT NULL,
    url             TEXT NOT NULL,
    sector          TEXT NOT NULL,
    priority        INTEGER NOT NULL DEFAULT 5,
    is_enabled      INTEGER NOT NULL DEFAULT 1,
    last_fetched    TEXT,
    last_error      TEXT
);

-- User profile
CREATE TABLE IF NOT EXISTS user_profile (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    key             TEXT NOT NULL UNIQUE,
    value           TEXT NOT NULL,
    scanned_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Chat history
CREATE TABLE IF NOT EXISTS chat_messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL,
    role            TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
    content         TEXT NOT NULL,
    sources         TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_session ON chat_messages(session_id, created_at);

-- Fetch log
CREATE TABLE IF NOT EXISTS fetch_log (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    date            TEXT NOT NULL,
    source_name     TEXT NOT NULL,
    articles_found  INTEGER NOT NULL DEFAULT 0,
    articles_after_dedup INTEGER NOT NULL DEFAULT 0,
    duration_ms     INTEGER,
    error           TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Seed feed configurations
INSERT OR IGNORE INTO feed_configs (name, feed_type, url, sector, priority) VALUES
    ('Google News AI', 'google_news', 'https://news.google.com/rss/search?q=artificial+intelligence+OR+LLM+OR+%22large+language+model%22+OR+ChatGPT+OR+Claude+OR+Gemini&hl=en-US&gl=US&ceid=US:en', 'ai', 8),
    ('Google News Miami', 'google_news', 'https://news.google.com/rss/search?q=%22Miami+Beach%22+OR+%22South+Beach%22+OR+%22Miami-Dade%22&hl=en-US&gl=US&ceid=US:en', 'miami', 8),
    ('Google News Italy', 'google_news', 'https://news.google.com/rss/search?q=Italy+politics+OR+Italian+government+OR+Meloni&hl=en&gl=US&ceid=US:en', 'italy', 8),
    ('Google News Tech', 'google_news', 'https://news.google.com/rss/search?q=technology+innovation+startup+electronics&hl=en-US&gl=US&ceid=US:en', 'tech', 8),
    ('OpenAI Blog', 'rss', 'https://openai.com/blog/rss/', 'ai', 10),
    ('Google AI Blog', 'rss', 'https://ai.googleblog.com/feeds/posts/default', 'ai', 10),
    ('DeepMind Blog', 'rss', 'https://deepmind.com/blog/feed/basic/', 'ai', 10),
    ('HuggingFace Blog', 'rss', 'https://huggingface.co/blog/feed.xml', 'ai', 9),
    ('ArXiv AI', 'rss', 'https://rss.arxiv.org/rss/cs.AI', 'ai', 6),
    ('ArXiv ML', 'rss', 'https://arxiv.org/rss/cs.LG', 'ai', 6),
    ('VentureBeat AI', 'rss', 'https://venturebeat.com/category/ai/feed/', 'ai', 7),
    ('TechCrunch', 'rss', 'https://techcrunch.com/feed/', 'tech', 8),
    ('The Verge', 'rss', 'https://www.theverge.com/rss/index.xml', 'tech', 7),
    ('ANSA Politica', 'rss', 'https://www.ansa.it/sito/notizie/politica/politica_rss.xml', 'italy', 9),
    ('La Repubblica Politica', 'rss', 'https://www.repubblica.it/rss/politica/rss2.0.xml', 'italy', 8),
    ('Corriere della Sera', 'rss', 'http://xml2.corriereobjects.it/rss/homepage.xml', 'italy', 7),
    ('WSVN Miami', 'rss', 'https://wsvn.com/feed', 'miami', 7),
    ('HN Front Page', 'hacker_news', 'https://hn.algolia.com/api/v1/search?tags=front_page&hitsPerPage=30', 'tech', 7),
    ('HN AI', 'hacker_news', 'https://hn.algolia.com/api/v1/search?query=AI+LLM+GPT+Claude&tags=story&hitsPerPage=20', 'ai', 7);
