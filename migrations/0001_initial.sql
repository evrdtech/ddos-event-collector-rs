CREATE TABLE IF NOT EXISTS pending_events (
        id INTEGER PRIMARY KEY,
        event_data TEXT NOT NULL,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS destinations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        broker TEXT NOT NULL CHECK(broker IN ('rabbitmq', 'kafka')),
        topic_queue TEXT NOT NULL,
        connection_url TEXT NOT NULL,
        enabled BOOLEAN NOT NULL DEFAULT 1,
        UNIQUE(broker, topic_queue, connection_url)
    );

CREATE TABLE IF NOT EXISTS config (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
);
