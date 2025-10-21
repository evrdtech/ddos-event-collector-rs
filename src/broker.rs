// broker.rs
use super::models::{Event, Destination, Broker};
use crate::config::NODE_INFO;
use lapin::{Connection, Channel};
use lapin::options::{QueueDeclareOptions, BasicPublishOptions};
use lapin::types::FieldTable;
use lapin::BasicProperties; // Added import for AMQPProperties
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::config::FromClientConfig;
use rdkafka::client::DefaultClientContext;
use rdkafka::util::TokioRuntime; // Added import for TokioRuntime
use serde_json;
use std::time::Duration;
use tracing::error;

pub async fn send_to_destination(event: &Event, dest: &Destination) -> bool {
    let mut event_with_node = event.clone();
    event_with_node.node_id = NODE_INFO.lock().unwrap().get_node_id();

    match dest.broker {
        Broker::Rabbitmq => send_to_rabbitmq(&event_with_node, dest).await,
        Broker::Kafka => send_to_kafka(&event_with_node, dest).await,
    }
}

async fn send_to_rabbitmq(event: &Event, dest: &Destination) -> bool {
    let conn_str = &dest.connection_url;
    match Connection::connect(conn_str, Default::default()).await {
        Ok(conn) => {
            match conn.create_channel().await {
                Ok(channel) => {
                    declare_queue(&channel, &dest.topic_queue).await;
                    let payload = match serde_json::to_vec(&event) {
                        Ok(p) => p,
                        Err(e) => {
                            error!("Failed to serialize event for RabbitMQ: {}", e);
                            return false;
                        }
                    };
                    match channel.basic_publish(
                        "".into(), // Convert to ShortString
                        dest.topic_queue.as_str().into(), // Convert to ShortString
                        BasicPublishOptions::default(),
                        &payload,
                        BasicProperties::default(), // Changed from FieldTable to BasicProperties
                    ).await {
                        Ok(_) => true,
                        Err(e) => {
                            error!("Failed to publish to RabbitMQ: {}", e);
                            false
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to create RabbitMQ channel: {}", e);
                    false
                }
            }
        }
        Err(e) => {
            error!("Failed to connect to RabbitMQ: {}", e);
            false
        }
    }
}

async fn declare_queue(channel: &Channel, queue: &str) {
    let _ = channel.queue_declare(
        queue.into(), // Convert to ShortString
        QueueDeclareOptions {
            durable: true,
            ..QueueDeclareOptions::default()
        },
        FieldTable::default(),
    ).await;
}

async fn send_to_kafka(event: &Event, dest: &Destination) -> bool {
    let mut config = rdkafka::ClientConfig::new();
    config.set("bootstrap.servers", &dest.connection_url);

    // Fixed: Explicitly specify the type for FutureProducer
    let producer: FutureProducer<DefaultClientContext, TokioRuntime> = match FutureProducer::from_config(&config) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to create Kafka producer: {}", e);
            return false;
        }
    };

    let payload = match serde_json::to_vec(event) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to serialize event for Kafka: {}", e);
            return false;
        }
    };

    let delivery_status = producer
        .send(
            FutureRecord::to(&dest.topic_queue)
                .payload(&payload)
                .key(&event.event_id),
            Duration::from_secs(3),
        )
        .await;

    match delivery_status {
        Ok(_) => true,
        Err((e, _)) => {
            error!("Failed to send to Kafka: {}", e);
            false
        }
    }
}