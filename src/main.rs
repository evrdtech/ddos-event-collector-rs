// main.rs
use anyhow::anyhow;
use rustls::crypto::ring;
use std::{env, io, net::SocketAddr};
use tokio::net::{lookup_host, TcpListener};
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

async fn resolve_listen_addr(value: &str, port: u16) -> io::Result<SocketAddr> {
    // Support IPv4/IPv6 literal addresses
    if let Ok(parsed_ip) = value.parse() {
        return Ok(SocketAddr::new(parsed_ip, port));
    }

    // Resolve hostnames (e.g., localhost) to available addresses
    let mut addresses: Vec<SocketAddr> = lookup_host((value, port)).await?.collect();

    if value.eq_ignore_ascii_case("localhost") {
        // Prefer IPv6, but fall back to IPv4 if only v4 is available
        addresses.sort_by_key(|addr| if addr.is_ipv6() { 0 } else { 1 });
    }

    addresses
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unable to resolve API_ADDR"))
}

fn fallback_address(current: &SocketAddr, fallback_port: u16) -> SocketAddr {
    SocketAddr::new(current.ip(), fallback_port)
}

#[derive(Debug)]
struct Args {
    host: String,
    port: u16,
}

impl Args {
    fn parse() -> Result<Self> {
        let mut host: Option<String> = None;
        let mut port: Option<u16> = None;

        for arg in env::args().skip(1) {
            if let Some(value) = arg.strip_prefix("--host=") {
                host = Some(value.to_string());
            } else if let Some(value) = arg.strip_prefix("--port=") {
                let parsed_port = value.parse::<u16>().map_err(|_| anyhow!("invalid port"))?;
                port = Some(parsed_port);
            }
        }

        Ok(Self {
            host: host.unwrap_or_else(|| "127.0.0.1".to_string()),
            port: port.unwrap_or(9000),
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .finish()
        .try_init()?;

    let pool = SqlitePool::connect("sqlite:events.db?mode=rwc").await?;

    ring::default_provider()
        .install_default()
        .expect("failed to install rustls ring crypto provider");
    init_db(&pool).await?;
    config::NodeInfo::load_from_db(&pool).await?;

    let app_state = AppState { pool: pool.clone() };

    tokio::spawn(retry_task(app_state.clone()));

    let args = Args::parse()?;

    let listen_addr = resolve_listen_addr(&args.host, args.port).await?;

    let listener = match TcpListener::bind(&listen_addr).await {
        Ok(listener) => listener,
        Err(_) => {
            // If the configured port is in use, try the next available port (wrap to default on overflow)
            let fallback_port = args.port.checked_add(1).unwrap_or(9000);
            let fallback_addr = fallback_address(&listen_addr, fallback_port);
            println!(
                "Port {} is in use for {}, trying {}",
                args.port, listen_addr, fallback_addr
            );
            TcpListener::bind(&fallback_addr).await?
        }
    };

    let actual_addr = listener.local_addr()?;

    let app = api::create_routes() // Use the create_routes function from api module
        .layer(axum::Extension(pool));

    println!("🚀 Servidor rodando em http://{}", actual_addr);
    println!("📖 Swagger UI available at http://{}/api/v1/docs/", actual_addr);

    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
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