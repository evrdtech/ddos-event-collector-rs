use super::models::{Broker, Destination, Event};
use crate::config::NODE_INFO;
use lapin::{
    options::{BasicPublishOptions, QueueDeclareOptions},
    types::FieldTable,
    BasicProperties, Channel, Connection, ConnectionProperties,
};
use rdkafka::{
    client::DefaultClientContext,
    config::FromClientConfig,
    producer::{FutureProducer, FutureRecord},
    util::TokioRuntime,
};
use serde_json;
use std::time::Duration;
use tracing::error;
use url::Url;

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
    match Connection::connect(conn_str, ConnectionProperties::default()).await {
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
                            let redacted = redact_amqp_password(conn_str);
                            error!("Failed to publish to RabbitMQ ({}): {}", redacted, e);
                            false
                        }
                    }
                }
                Err(e) => {
                    let redacted = redact_amqp_password(conn_str);
                    error!("Failed to create RabbitMQ channel ({}): {}", redacted, e);
                    false
                }
            }
        }
        Err(e) => {
            let redacted = redact_amqp_password(conn_str);
            error!("Failed to connect to RabbitMQ ({}): {}", redacted, e);
            false
        }
    }
}

fn redact_amqp_password(url: &str) -> String {
    const PASSWORD_MASK: &str = "***********";
    match Url::parse(url) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some(PASSWORD_MASK));
                parsed.into()
            } else {
                url.to_owned()
            }
        }
        Err(_) => url.to_owned(),
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
