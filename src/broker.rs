// broker.rs
use super::models::{Broker, Destination, Event};
use crate::config::NODE_INFO;
use lapin::{options::{BasicPublishOptions, QueueDeclareOptions}, types::FieldTable, BasicProperties, Channel, Connection, ConnectionProperties};
use rdkafka::client::DefaultClientContext;
use rdkafka::config::FromClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::util::TokioRuntime; // Added import for TokioRuntime
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier}, ClientConfig, RootCertStore};
use serde_json;
use std::sync::Arc;
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
    match connect_with_tls_options(conn_str, dest.allow_invalid_tls).await {
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

struct AllowInvalidCertVerifier;

impl ServerCertVerifier for AllowInvalidCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
}

async fn connect_with_tls_options(uri: &str, allow_invalid_tls: bool) -> Result<Connection, lapin::Error> {
    let builder = lapin::Connection::builder();

    if !uri.starts_with("amqp") {
        return builder.connection_properties(ConnectionProperties::default()).connect(uri).await;
    }

    if allow_invalid_tls {
        let mut root_store = RootCertStore::empty();
        let client_config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let client_config = Arc::new(client_config)
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AllowInvalidCertVerifier));

        builder
            .connection_properties(ConnectionProperties::default().with_tls(client_config))
            .connect(uri)
            .await
    } else {
        builder.connection_properties(ConnectionProperties::default()).connect(uri).await
    }
}