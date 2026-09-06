use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use radar_api::{FusionRequest, JobRequest, JobState, JobStatus, Radar};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{RwLock, Semaphore};
use tower_http::{cors::CorsLayer, limit::RequestBodyLimitLayer};
use uuid::Uuid;
const DEFAULT_MAX_RADARS: usize = 32;
const DEFAULT_MAX_CELLS: u64 = 100_000_000;
#[derive(Clone)]
struct JobRecord {
    status: JobStatus,
    cancelled: Arc<AtomicBool>,
}
#[derive(Clone)]
struct App {
    radars: Arc<RwLock<HashMap<Uuid, Radar>>>,
    jobs: Arc<RwLock<HashMap<Uuid, JobRecord>>>,
    job_slots: Arc<Semaphore>,
    max_radars: usize,
    max_cells: u64,
}
impl Default for App {
    fn default() -> Self {
        Self {
            radars: Default::default(),
            jobs: Default::default(),
            job_slots: Arc::new(Semaphore::new(env_usize("RADAR_MAX_CONCURRENT_JOBS", 2))),
            max_radars: env_usize("RADAR_MAX_RADARS", DEFAULT_MAX_RADARS),
            max_cells: env_u64("RADAR_MAX_GRID_CELLS", DEFAULT_MAX_CELLS),
        }
    }
}
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().json().init();
    let state = App::default();
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(|| async { "ready" }))
        .route("/api/v1/radars", get(list_radars).post(create_radar))
        .route("/api/v1/radars/{id}", get(get_radar).put(update_radar))
        .route("/api/v1/jobs", post(create_job))
        .route("/api/v1/jobs/{id}", get(get_job).delete(cancel_job))
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
async fn list_radars(State(s): State<App>) -> Json<Vec<Radar>> {
    Json(s.radars.read().await.values().cloned().collect())
}
async fn create_radar(
    State(s): State<App>,
    Json(r): Json<Radar>,
) -> ApiResult<(StatusCode, Json<Radar>)> {
    validate_radar(&r)?;
    s.radars.write().await.insert(r.id, r.clone());
    Ok((StatusCode::CREATED, Json(r)))
}
async fn update_radar(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    Json(mut r): Json<Radar>,
) -> ApiResult<Json<Radar>> {
    validate_radar(&r)?;
    if !s.radars.read().await.contains_key(&id) {
        return Err(public_error(StatusCode::NOT_FOUND, "radar not found"));
    }
    r.id = id;
    s.radars.write().await.insert(id, r.clone());
    Ok(Json(r))
}
async fn get_radar(State(s): State<App>, Path(id): Path<Uuid>) -> ApiResult<Json<Radar>> {
    s.radars
        .read()
        .await
        .get(&id)
        .cloned()
        .map(Json)
        .ok_or_else(|| public_error(StatusCode::NOT_FOUND, "radar not found"))
}
async fn create_job(
    State(s): State<App>,
    Json(request): Json<JobRequest>,
) -> ApiResult<(StatusCode, Json<JobStatus>)> {
    let estimate = request
        .validate(s.max_radars, s.max_cells)
        .map_err(|e| public_error(StatusCode::BAD_REQUEST, e))?;
    let max_memory = env_u64("RADAR_MEMORY_BUDGET_BYTES", 2 * 1024 * 1024 * 1024);
    if estimate.bytes_per_radar > max_memory {
        return Err(public_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "job exceeds memory budget",
        ));
    }
    let id = Uuid::new_v4();
    let status = JobStatus {
        id,
        state: JobState::Queued,
        progress: 0.,
        created_at: now(),
        started_at: None,
        finished_at: None,
        error: None,
        estimate,
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    s.jobs.write().await.insert(
        id,
        JobRecord {
            status: status.clone(),
            cancelled: cancelled.clone(),
        },
    );
    tokio::spawn(run_job(s.clone(), id, cancelled));
    Ok((StatusCode::ACCEPTED, Json(status)))
}
async fn run_job(s: App, id: Uuid, cancelled: Arc<AtomicBool>) {
    let Ok(_permit) = s.job_slots.acquire().await else {
        return;
    };
    if cancelled.load(Ordering::Acquire) {
        set_cancelled(&s, id).await;
        return;
    }
    {
        let mut jobs = s.jobs.write().await;
        if let Some(job) = jobs.get_mut(&id) {
            job.status.state = JobState::Running;
            job.status.started_at = Some(now());
            job.status.progress = 0.01
        }
    }
    let mut jobs = s.jobs.write().await;
    if let Some(job) = jobs.get_mut(&id) {
        if cancelled.load(Ordering::Acquire) {
            job.status.state = JobState::Cancelled;
            job.status.error = None
        } else {
            job.status.state = JobState::Failed;
            job.status.error = Some("terrain reprojection pipeline is not configured".into())
        }
        job.status.finished_at = Some(now())
    }
}
async fn get_job(State(s): State<App>, Path(id): Path<Uuid>) -> ApiResult<Json<JobStatus>> {
    s.jobs
        .read()
        .await
        .get(&id)
        .map(|j| Json(j.status.clone()))
        .ok_or_else(|| public_error(StatusCode::NOT_FOUND, "job not found"))
}
async fn cancel_job(State(s): State<App>, Path(id): Path<Uuid>) -> ApiResult<Json<JobStatus>> {
    let cancelled = {
        let jobs = s.jobs.read().await;
        jobs.get(&id)
            .map(|j| j.cancelled.clone())
            .ok_or_else(|| public_error(StatusCode::NOT_FOUND, "job not found"))?
    };
    cancelled.store(true, Ordering::Release);
    set_cancelled(&s, id).await;
    get_job(State(s), Path(id)).await
}
async fn set_cancelled(s: &App, id: Uuid) {
    if let Some(job) = s.jobs.write().await.get_mut(&id) {
        if matches!(job.status.state, JobState::Queued | JobState::Running) {
            job.status.state = JobState::Cancelled;
            job.status.finished_at = Some(now());
            job.status.error = None
        }
    }
}
async fn fusion(Json(req): Json<FusionRequest>) -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"selected_radars":req.radar_ids,"target_agl_m":req.target_agl_m,"status":"queued"}),
    )
}
type ApiResult<T> = Result<T, (StatusCode, Json<serde_json::Value>)>;
fn public_error(status: StatusCode, message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (status, Json(serde_json::json!({"error":message})))
}
fn validate_radar(r: &Radar) -> ApiResult<()> {
    r.validate()
        .map_err(|e| public_error(StatusCode::BAD_REQUEST, e))
}
fn now() -> String {
    Utc::now().to_rfc3339()
}
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
