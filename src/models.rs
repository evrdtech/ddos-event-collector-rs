use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Deserialize, ToSchema)]
pub struct InputEvent {
    pub event_type: String,
    pub data: Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, ToSchema)]
pub struct Event {
    pub event_id: String,
    pub event_type: String,
    pub data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub received_at: DateTime<Utc>,
}

impl Event {
    pub fn new(input: InputEvent) -> Self {
        Self {
            event_id: Uuid::new_v4().to_string(),
            event_type: input.event_type,
            data: input.data,
            node_id: None,
            received_at: Utc::now(),
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct EventResponse {
    pub status: String,
    pub event_id: String,
    pub received_at: DateTime<Utc>,
}

#[derive(sqlx::Type, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[sqlx(type_name = "broker")]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Broker {
    Rabbitmq,
    Kafka,
}

impl std::fmt::Display for Broker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Broker::Rabbitmq => write!(f, "rabbitmq"),
            Broker::Kafka => write!(f, "kafka"),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct Destination {
    pub id: i64,
    pub broker: Broker,
    pub topic_queue: String,
    pub connection_url: String,
    pub enabled: bool,
    #[serde(default)]
    pub allow_invalid_tls: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct NewDestination {
    pub broker: Broker,
    pub topic_queue: String,
    pub connection_url: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub allow_invalid_tls: bool,
}

// Function to provide default value for enabled field
fn default_enabled() -> bool {
    true
}
