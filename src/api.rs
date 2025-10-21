use axum::{
    routing::{get, post, put, delete},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    Json, Router, extract::Extension, body::Body, extract::Path,
};
use serde::{Serialize, Deserialize};
use serde_json::json;
use utoipa::OpenApi;
use utoipa::ToSchema;
use sqlx::{SqlitePool, Row};
use chrono::Utc;

use crate::models::*;

// Helper function to check if node_id is set
async fn check_node_id_set() -> Result<(), Response> {
    if !crate::config::NODE_INFO.lock().unwrap().is_node_id_set() {
        let error_response = json!({
            "error": "Node ID not set",
            "details": "The node ID must be set before using this endpoint"
        });
        return Err(Response::builder()
            .status(StatusCode::PRECONDITION_REQUIRED)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
            .unwrap());
    }
    Ok(())
}

// Macro to automatically create the OpenAPI documentation
macro_rules! create_openapi_doc {
    ($($path:ident),* ; $($schema:ident),*) => {
        #[derive(OpenApi)]
        #[openapi(
            paths(
                $($path),*
            ),
            components(
                schemas(
                    $($schema),*
                )
            ),
            tags(
                (name = "Event Collector", description = "API for collecting and managing events")
            )
        )]
        struct ApiDoc;
    };
}

// Automatically create the OpenAPI documentation with all paths and schemas
create_openapi_doc!(
    webhook_handler,
    list_destinations,
    add_destination,
    update_destination,
    delete_destination,
    get_status,
    set_node_info,
    force_retry;
    InputEvent,
    Event,
    EventResponse,
    Broker,
    Destination,
    NewDestination,
    NodeInfoRequest,
    StatusResponse,
    NodeInfo
);

// Handler for redirecting to Swagger UI with trailing slash
async fn redirect_to_swagger() -> impl IntoResponse {
    (
        StatusCode::TEMPORARY_REDIRECT,
        [(header::LOCATION, "/swagger-ui/")],
        "Redirecting to Swagger UI..."
    )
}

// Handler for serving the Swagger UI HTML
async fn swagger_ui_handler() -> Response {
    let html = r#"<!DOCTYPE html>
    <html>
    <head>
        <title>API Documentation</title>
        <link rel="stylesheet" type="text/css" href="https://unpkg.com/swagger-ui-dist@4.15.5/swagger-ui.css" />
        <style>
            html { box-sizing: border-box; overflow: -moz-scrollbars-vertical; overflow-y: scroll; }
            *, *:before, *:after { box-sizing: inherit; }
            body { margin: 0; background: #fafafa; }
        </style>
    </head>
    <body>
        <div id="swagger-ui"></div>
        <script src="https://unpkg.com/swagger-ui-dist@4.15.5/swagger-ui-bundle.js"></script>
        <script src="https://unpkg.com/swagger-ui-dist@4.15.5/swagger-ui-standalone-preset.js"></script>
        <script>
            window.onload = function() {
                const ui = SwaggerUIBundle({
                    url: '/api-docs/openapi.json',
                    dom_id: '#swagger-ui',
                    deepLinking: true,
                    presets: [
                        SwaggerUIBundle.presets.apis,
                        SwaggerUIStandalonePreset
                    ],
                    plugins: [
                        SwaggerUIBundle.plugins.DownloadUrl
                    ],
                    layout: "StandaloneLayout"
                });
            };
        </script>
    </body>
    </html>"#;

    Response::builder()
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(html))
        .unwrap()
}

// Handler for serving the OpenAPI JSON specification
async fn openapi_handler() -> Json<utoipa::openapi::OpenApi> {
    let doc = ApiDoc::openapi();
    Json(doc)
}

#[utoipa::path(
    post,
    path = "/api/v1/webhook",
    request_body = InputEvent,
    responses(
        (status = 202, description = "Event accepted", body = EventResponse),
        (status = 500, description = "Internal server error", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn webhook_handler(
    Extension(pool): Extension<SqlitePool>,
    Json(input): Json<InputEvent>,
) -> impl IntoResponse {
    // Webhook endpoint doesn't require node_id to be set
    let event = Event::new(input);

    let event_json = match serde_json::to_string(&event) {
        Ok(s) => s,
        Err(_) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Failed to serialize event"}))
        ),
    };

    if let Err(e) = sqlx::query("INSERT INTO pending_events (event_data) VALUES (?)")
        .bind(&event_json)
        .execute(&pool)
        .await
    {
        tracing::error!("Falha ao salvar evento: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Database error"}))
        );
    }

    // Convert EventResponse to Value to ensure consistent return type
    let response = EventResponse {
        status: "accepted".to_string(),
        event_id: event.event_id,
        received_at: event.received_at
    };

    let response_json = match serde_json::to_value(response) {
        Ok(val) => val,
        Err(_) => json!({"error": "Failed to create response"}),
    };

    (StatusCode::ACCEPTED, Json(response_json))
}

#[utoipa::path(
    get,
    path = "/api/v1/destinations",
    responses(
        (status = 200, description = "List of destinations", body = Vec<Destination>),
        (status = 412, description = "Node ID not set", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn list_destinations(Extension(pool): Extension<SqlitePool>) -> Response {
    // Check if node_id is set
    if let Err(response) = check_node_id_set().await {
        return response;
    }

    let dests = sqlx::query_as::<_, Destination>("SELECT id, broker, topic_queue, connection_url, enabled FROM destinations WHERE enabled = 1")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();

    // Return Json directly, not as part of a tuple
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&dests).unwrap()))
        .unwrap()
}

#[utoipa::path(
    post,
    path = "/api/v1/destinations",
    request_body = NewDestination,
    responses(
        (status = 201, description = "Destination created", body = Destination),
        (status = 400, description = "Bad request - validation error", body = serde_json::Value),
        (status = 409, description = "Conflict - destination already exists", body = serde_json::Value),
        (status = 412, description = "Node ID not set", body = serde_json::Value),
        (status = 500, description = "Internal server error", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn add_destination(
    Extension(pool): Extension<SqlitePool>,
    Json(new_dest): Json<NewDestination>,
) -> Response {
    // Check if node_id is set
    if let Err(response) = check_node_id_set().await {
        return response;
    }

    // Insert the destination and get the last inserted ID
    let result = sqlx::query(
        "INSERT INTO destinations (broker, topic_queue, connection_url, enabled) VALUES (?, ?, ?, ?)"
    )
        .bind(new_dest.broker.to_string())
        .bind(&new_dest.topic_queue)
        .bind(&new_dest.connection_url)
        .bind(new_dest.enabled)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            // Try to get the newly created destination by matching on unique fields
            match sqlx::query_as::<_, Destination>(
                "SELECT id, broker, topic_queue, connection_url, enabled FROM destinations WHERE broker = ? AND topic_queue = ? AND connection_url = ? ORDER BY id DESC LIMIT 1"
            )
                .bind(new_dest.broker.to_string())
                .bind(&new_dest.topic_queue)
                .bind(&new_dest.connection_url)
                .fetch_one(&pool)
                .await
            {
                Ok(dest) => {
                    // Create a successful response with the destination
                    let json_response = Json(dest);
                    let response = Response::builder()
                        .status(StatusCode::CREATED)
                        .header(header::CONTENT_TYPE, "application/json");

                    // Try to serialize the destination to JSON
                    match serde_json::to_value(&json_response.0) {
                        Ok(_) => response.body(Body::from(serde_json::to_vec(&json_response.0).unwrap())).unwrap(),
                        Err(e) => {
                            tracing::error!("Failed to serialize destination: {}", e);
                            let error_response = json!({
                                "error": "Failed to serialize destination",
                                "details": e.to_string()
                            });
                            Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .header(header::CONTENT_TYPE, "application/json")
                                .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                                .unwrap()
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to retrieve created destination: {}", e);

                    // Try to get all destinations to see if any were inserted
                    match sqlx::query_as::<_, Destination>(
                        "SELECT id, broker, topic_queue, connection_url, enabled FROM destinations ORDER BY id DESC LIMIT 5"
                    )
                        .fetch_all(&pool)
                        .await
                    {
                        Ok(all_dests) => {
                            tracing::error!("Available destinations: {:?}", all_dests);
                        }
                        Err(e2) => {
                            tracing::error!("Failed to fetch any destinations: {}", e2);
                        }
                    }

                    let error_response = json!({
                        "error": "Failed to retrieve created destination",
                        "details": e.to_string()
                    });
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                        .unwrap()
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to create destination: {}", e);

            // Check if it's a UNIQUE constraint violation
            if e.to_string().contains("UNIQUE constraint failed") {
                let error_response = json!({
                    "error": "Destination already exists",
                    "details": "A destination with the same broker, topic_queue, and connection_url already exists"
                });
                Response::builder()
                    .status(StatusCode::CONFLICT) // 409 Conflict
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                    .unwrap()
            } else if e.to_string().contains("NOT NULL constraint failed") {
                let error_response = json!({
                    "error": "Validation error",
                    "details": "Required fields cannot be empty"
                });
                Response::builder()
                    .status(StatusCode::BAD_REQUEST) // 400 Bad Request
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                    .unwrap()
            } else if e.to_string().contains("CHECK constraint failed") {
                let error_response = json!({
                    "error": "Invalid broker value",
                    "details": "Broker must be either 'rabbitmq' or 'kafka'"
                });
                Response::builder()
                    .status(StatusCode::BAD_REQUEST) // 400 Bad Request
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                    .unwrap()
            } else {
                // For any other error, use 500 Internal Server Error
                let error_response = json!({
                    "error": "Failed to create destination",
                    "details": e.to_string()
                });
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                    .unwrap()
            }
        }
    }
}

#[utoipa::path(
    put,
    path = "/api/v1/destinations/{id}",
    params(("id", description = "Destination ID")),
    request_body = NewDestination,
    responses(
        (status = 200, description = "Destination updated", body = Destination),
        (status = 400, description = "Bad request - validation error", body = serde_json::Value),
        (status = 404, description = "Destination not found", body = serde_json::Value),
        (status = 412, description = "Node ID not set", body = serde_json::Value),
        (status = 500, description = "Internal server error", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn update_destination(
    Extension(pool): Extension<SqlitePool>,
    Path(id): Path<i64>,
    Json(update_dest): Json<NewDestination>,
) -> Response {
    // Check if node_id is set
    if let Err(response) = check_node_id_set().await {
        return response;
    }

    // Check if the destination exists
    match sqlx::query_as::<_, Destination>(
        "SELECT id, broker, topic_queue, connection_url, enabled FROM destinations WHERE id = ?"
    )
        .bind(id)
        .fetch_one(&pool)
        .await
    {
        Ok(_) => {
            // Update the destination
            let result = sqlx::query(
                "UPDATE destinations SET broker = ?, topic_queue = ?, connection_url = ?, enabled = ? WHERE id = ?"
            )
                .bind(update_dest.broker.to_string())
                .bind(&update_dest.topic_queue)
                .bind(&update_dest.connection_url)
                .bind(update_dest.enabled)
                .bind(id)
                .execute(&pool)
                .await;

            match result {
                Ok(_) => {
                    // Get the updated destination
                    match sqlx::query_as::<_, Destination>(
                        "SELECT id, broker, topic_queue, connection_url, enabled FROM destinations WHERE id = ?"
                    )
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                    {
                        Ok(dest) => {
                            let json_response = Json(dest);
                            let response = Response::builder()
                                .status(StatusCode::OK)
                                .header(header::CONTENT_TYPE, "application/json");

                            match serde_json::to_value(&json_response.0) {
                                Ok(_) => response.body(Body::from(serde_json::to_vec(&json_response.0).unwrap())).unwrap(),
                                Err(e) => {
                                    tracing::error!("Failed to serialize destination: {}", e);
                                    let error_response = json!({
                                        "error": "Failed to serialize destination",
                                        "details": e.to_string()
                                    });
                                    Response::builder()
                                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                                        .header(header::CONTENT_TYPE, "application/json")
                                        .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                                        .unwrap()
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to retrieve updated destination: {}", e);
                            let error_response = json!({
                                "error": "Failed to retrieve updated destination",
                                "details": e.to_string()
                            });
                            Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .header(header::CONTENT_TYPE, "application/json")
                                .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                                .unwrap()
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to update destination: {}", e);

                    if e.to_string().contains("UNIQUE constraint failed") {
                        let error_response = json!({
                            "error": "Destination already exists",
                            "details": "A destination with the same broker, topic_queue, and connection_url already exists"
                        });
                        Response::builder()
                            .status(StatusCode::CONFLICT)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                            .unwrap()
                    } else if e.to_string().contains("NOT NULL constraint failed") {
                        let error_response = json!({
                            "error": "Validation error",
                            "details": "Required fields cannot be empty"
                        });
                        Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                            .unwrap()
                    } else {
                        let error_response = json!({
                            "error": "Failed to update destination",
                            "details": e.to_string()
                        });
                        Response::builder()
                            .status(StatusCode::INTERNAL_SERVER_ERROR)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                            .unwrap()
                    }
                }
            }
        }
        Err(_) => {
            let error_response = json!({
                "error": "Destination not found",
                "details": format!("No destination found with ID {}", id)
            });
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                .unwrap()
        }
    }
}

#[utoipa::path(
    delete,
    path = "/api/v1/destinations/{id}",
    params(("id", description = "Destination ID")),
    responses(
        (status = 204, description = "Destination deleted"),
        (status = 404, description = "Destination not found", body = serde_json::Value),
        (status = 412, description = "Node ID not set", body = serde_json::Value),
        (status = 500, description = "Internal server error", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn delete_destination(
    Extension(pool): Extension<SqlitePool>,
    Path(id): Path<i64>,
) -> Response {
    // Check if node_id is set
    if let Err(response) = check_node_id_set().await {
        return response;
    }

    // Check if the destination exists
    match sqlx::query_as::<_, Destination>(
        "SELECT id, broker, topic_queue, connection_url, enabled FROM destinations WHERE id = ?"
    )
        .bind(id)
        .fetch_one(&pool)
        .await
    {
        Ok(_) => {
            // Delete the destination
            match sqlx::query("DELETE FROM destinations WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await
            {
                Ok(_) => {
                    Response::builder()
                        .status(StatusCode::NO_CONTENT)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::empty())
                        .unwrap()
                }
                Err(e) => {
                    tracing::error!("Failed to delete destination: {}", e);
                    let error_response = json!({
                        "error": "Failed to delete destination",
                        "details": e.to_string()
                    });
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                        .unwrap()
                }
            }
        }
        Err(_) => {
            let error_response = json!({
                "error": "Destination not found",
                "details": format!("No destination found with ID {}", id)
            });
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                .unwrap()
        }
    }
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct StatusResponse {
    pub status: String,
    pub active_destinations: i64,
    pub total_destinations: i64,
    pub pending_events: i64,
    pub node_info: NodeInfo,
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct NodeInfo {
    pub node_id: Option<String>,
    pub version: String,
    pub region: String,
    pub name: String,
    pub started_at: String,
}

#[utoipa::path(
    get,
    path = "/api/v1/status",
    responses(
        (status = 200, description = "System status", body = StatusResponse),
        (status = 412, description = "Node ID not set", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn get_status(
    Extension(pool): Extension<SqlitePool>,
) -> Response {
    // Check if node_id is set
    if let Err(response) = check_node_id_set().await {
        return response;
    }

    // Get counts
    let total_destinations_row = sqlx::query("SELECT COUNT(*) as count FROM destinations")
        .fetch_one(&pool)
        .await
        .unwrap();
    let total_destinations: i64 = total_destinations_row.get::<i64, _>("count");

    let active_destinations_row = sqlx::query("SELECT COUNT(*) as count FROM destinations WHERE enabled = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let active_destinations: i64 = active_destinations_row.get::<i64, _>("count");

    let pending_events_row = sqlx::query("SELECT COUNT(*) as count FROM pending_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    let pending_events: i64 = pending_events_row.get::<i64, _>("count");

    // Get node info from config
    let node_id = crate::config::NODE_INFO.lock().unwrap().get_node_id();

    let status = StatusResponse {
        status: "running".to_string(),
        active_destinations,
        total_destinations,
        pending_events,
        node_info: NodeInfo {
            node_id,
            version: "1.1.0".to_string(),
            region: "sa-east-1".to_string(),
            name: "node-01".to_string(),
            started_at: Utc::now().to_rfc3339(),
        },
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&status).unwrap()))
        .unwrap()
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct NodeInfoRequest {
    pub node_id: Option<String>,
    pub name: String,
    pub region: String,
}

#[utoipa::path(
    put,
    path = "/api/v1/node-info",
    request_body = NodeInfoRequest,
    responses(
        (status = 200, description = "Node info updated", body = NodeInfo),
        (status = 500, description = "Internal server error", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn set_node_info(
    Extension(pool): Extension<SqlitePool>,
    Json(node_info_req): Json<NodeInfoRequest>,
) -> Response {
    // Update node info in config
    {
        let mut lock = crate::config::NODE_INFO.lock().unwrap();
        lock.set_node_id(node_info_req.node_id.clone());
    }

    // Save node info to database
    let result = sqlx::query(
        "INSERT OR REPLACE INTO config (key, value) VALUES (?, ?), (?, ?), (?, ?)"
    )
        .bind("node_id")
        .bind(node_info_req.node_id.as_ref().map_or("", |s| s.as_str()))
        .bind("node_name")
        .bind(&node_info_req.name)
        .bind("node_region")
        .bind(&node_info_req.region)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            let node_info = NodeInfo {
                node_id: node_info_req.node_id,
                version: "1.1.0".to_string(),
                region: node_info_req.region,
                name: node_info_req.name,
                started_at: Utc::now().to_rfc3339(),
            };

            let json_response = Json(node_info);
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&json_response.0).unwrap()))
                .unwrap()
        }
        Err(e) => {
            tracing::error!("Failed to save node info: {}", e);
            let error_response = json!({
                "error": "Failed to save node info",
                "details": e.to_string()
            });
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&error_response).unwrap()))
                .unwrap()
        }
    }
}

#[utoipa::path(
    post,
    path = "/api/v1/retry",
    responses(
        (status = 200, description = "Retry process initiated"),
        (status = 412, description = "Node ID not set", body = serde_json::Value)
    ),
    tag = "Event Collector"
)]
pub async fn force_retry(Extension(pool): Extension<SqlitePool>) -> Response {
    // Check if node_id is set
    if let Err(response) = check_node_id_set().await {
        return response;
    }

    crate::retry_pending_events(&pool).await;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"message":"Retry initiated"}"#))
        .unwrap()
}

// Function to create the API router with versioning
pub fn create_routes() -> Router {
    // Create a router for v1 API
    let v1_router = Router::new()
        .route("/webhook", post(webhook_handler))
        .route("/destinations", get(list_destinations))
        .route("/destinations", post(add_destination))
        .route("/destinations/{id}", put(update_destination))
        .route("/destinations/{id}", delete(delete_destination))
        .route("/status", get(get_status))
        .route("/node-info", put(set_node_info))
        .route("/retry", post(force_retry));

    Router::new()
        .route("/swagger-ui", axum::routing::get(redirect_to_swagger))
        .route("/swagger-ui/", axum::routing::get(swagger_ui_handler))
        .route("/api-docs/openapi.json", axum::routing::get(openapi_handler))
        .nest("/api/v1", v1_router) // Nest the v1 router under /api/v1
}