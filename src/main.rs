// main.rs
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::util::SubscriberInitExt;

mod models;
mod db;
mod broker;
mod config;
mod api;

use models::*;
use db::init_db;
use sqlx::SqlitePool;

type Result<T> = std::result::Result<T, anyhow::Error>;

#[derive(Clone)]
struct AppState {
    pool: SqlitePool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .finish()
        .try_init()?;

    let pool = SqlitePool::connect("sqlite:events.db?mode=rwc").await?;
    init_db(&pool).await?;
    config::NodeInfo::load_from_db(&pool).await?;

    let app_state = AppState { pool: pool.clone() };

    tokio::spawn(retry_task(app_state.clone()));

    // Try to use port 9003, but if it's in use, try port 9004
    let port = 9003;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(_) => {
            // If port 9003 is in use, try port 9004
            let fallback_addr = SocketAddr::from(([127, 0, 0, 1], 9004));
            println!("Port {} is in use, trying port 9004", port);
            TcpListener::bind(&fallback_addr).await?
        }
    };

    let actual_addr = listener.local_addr()?;

    let app = api::create_routes() // Use the create_routes function from api module
        .layer(axum::Extension(pool));

    println!("🚀 Servidor rodando em http://{}", actual_addr);
    println!("📖 Swagger UI available at http://{}/swagger-ui", actual_addr);

    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}

async fn retry_task(state: AppState) {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
        retry_pending_events(&state.pool).await;
    }
}

// Make this function public so it can be used in api.rs
pub async fn retry_pending_events(pool: &SqlitePool) {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT id, event_data FROM pending_events")
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    let mut success_ids = vec![];

    for (db_id, json_str) in rows {
        let event: Event = match serde_json::from_str(&json_str) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let destinations: Vec<Destination> = sqlx::query_as::<_, Destination>(
            "SELECT id, broker, topic_queue, connection_url, enabled FROM destinations WHERE enabled = 1"
        )
            .fetch_all(pool)
            .await
            .unwrap_or_default();

        let mut sent = false;
        for dest in &destinations {
            if broker::send_to_destination(&event, dest).await {
                sent = true;
            }
        }

        if sent {
            success_ids.push(db_id);
        }
    }

    if !success_ids.is_empty() {
        let placeholders = "?,".repeat(success_ids.len());
        let placeholders_trimmed = placeholders.trim_end_matches(',');
        let query_str = format!("DELETE FROM pending_events WHERE id IN ({})", placeholders_trimmed);

        let mut query = sqlx::query(&query_str);
        for id in success_ids {
            query = query.bind(id);
        }
        query.execute(pool).await.ok();
    }
}