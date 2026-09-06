use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use radar_api::{FusionRequest, Radar};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer};
use uuid::Uuid;
#[derive(Clone, Default)]
struct App {
    radars: Arc<RwLock<HashMap<Uuid, Radar>>>,
}
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();
    let state = App::default();
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ready" }))
        .route("/api/v1/radars", get(list).post(create))
        .route("/api/v1/radars/{id}", get(get_one))
        .route("/api/v1/fusions", post(fusion))
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(CorsLayer::permissive())
        .with_state(state);
    let addr = std::env::var("RADAR_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind server");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve")
}
async fn list(State(s): State<App>) -> Json<Vec<Radar>> {
    Json(s.radars.read().await.values().cloned().collect())
}
async fn create(
    State(s): State<App>,
    Json(r): Json<Radar>,
) -> Result<(StatusCode, Json<Radar>), (StatusCode, String)> {
    r.validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.into()))?;
    s.radars.write().await.insert(r.id, r.clone());
    Ok((StatusCode::CREATED, Json(r)))
}
async fn get_one(State(s): State<App>, Path(id): Path<Uuid>) -> Result<Json<Radar>, StatusCode> {
    s.radars
        .read()
        .await
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
async fn fusion(Json(req): Json<FusionRequest>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"selected_radars":req.radar_ids,"target_agl_m":req.target_agl_m,"status":"queued"}),
    )
}
