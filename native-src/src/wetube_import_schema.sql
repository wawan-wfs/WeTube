-- Test fixture: relevant table definitions from pinned upstream src/store.rs (MIT).
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE subscriptions (channel_id TEXT PRIMARY KEY, name TEXT NOT NULL, avatar TEXT, added_at INTEGER NOT NULL);
CREATE TABLE history (video_id TEXT PRIMARY KEY, json TEXT NOT NULL, watched_at INTEGER NOT NULL, progress REAL NOT NULL DEFAULT 0, duration REAL NOT NULL DEFAULT 0);
CREATE TABLE playlists (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, created_at INTEGER NOT NULL, protected INTEGER NOT NULL DEFAULT 0, yt_id TEXT);
CREATE TABLE playlist_items (playlist_id INTEGER NOT NULL, video_id TEXT NOT NULL, json TEXT NOT NULL, added_at INTEGER NOT NULL, position INTEGER NOT NULL, PRIMARY KEY(playlist_id, video_id));
CREATE TABLE search_history (query TEXT PRIMARY KEY, at INTEGER NOT NULL);
