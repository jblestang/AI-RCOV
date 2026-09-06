use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use coverage_core::{compute_coverage, Grid, LosConfig};
use coverage_storage::{write_rcov, write_rhgt, Metadata};
use radar_api::{FusionRequest, JobRequest, JobState, JobStatus, Radar};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use terrain_srtm::{tiles_for_radius, LocalProjection, MetricRaster, SrtmCache, SrtmCacheConfig};
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
    terrain: Arc<SrtmCache>,
    result_directory: Arc<std::path::PathBuf>,
}
impl Default for App {
    fn default() -> Self {
        Self {
            radars: Default::default(),
            jobs: Default::default(),
            job_slots: Arc::new(Semaphore::new(env_usize("RADAR_MAX_CONCURRENT_JOBS", 2))),
            max_radars: env_usize("RADAR_MAX_RADARS", DEFAULT_MAX_RADARS),
            max_cells: env_u64("RADAR_MAX_GRID_CELLS", DEFAULT_MAX_CELLS),
            terrain: Arc::new(
                SrtmCache::new(SrtmCacheConfig {
                    disk_directory: std::env::var("RADAR_SRTM_CACHE")
                        .map_or_else(|_| "data/srtm".into(), Into::into),
                    ..Default::default()
                })
                .expect("valid SRTM cache configuration"),
            ),
            result_directory: Arc::new(
                std::env::var("RADAR_RESULTS").map_or_else(|_| "data/results".into(), Into::into),
            ),
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
    tokio::spawn(run_job(s.clone(), id, cancelled, request));
    Ok((StatusCode::ACCEPTED, Json(status)))
}
async fn run_job(s: App, id: Uuid, cancelled: Arc<AtomicBool>, request: JobRequest) {
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
    let result = execute_job(&s, id, &request, &cancelled).await;
    let mut jobs = s.jobs.write().await;
    if let Some(job) = jobs.get_mut(&id) {
        if cancelled.load(Ordering::Acquire) {
            job.status.state = JobState::Cancelled;
            job.status.error = None
        } else if let Err(message) = result {
            job.status.state = JobState::Failed;
            job.status.error = Some(message)
        } else {
            job.status.state = JobState::Completed;
            job.status.progress = 1.0
        }
        job.status.finished_at = Some(now())
    }
}

async fn execute_job(
    s: &App,
    id: Uuid,
    request: &JobRequest,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    std::fs::create_dir_all(s.result_directory.as_ref())
        .map_err(|_| "cannot prepare result storage".to_owned())?;
    let center_lat =
        request.radars.iter().map(|r| r.latitude).sum::<f64>() / request.radars.len() as f64;
    let sin_lon = request
        .radars
        .iter()
        .map(|r| r.longitude.to_radians().sin())
        .sum::<f64>();
    let cos_lon = request
        .radars
        .iter()
        .map(|r| r.longitude.to_radians().cos())
        .sum::<f64>();
    let center_lon = sin_lon.atan2(cos_lon).to_degrees();
    let projection = LocalProjection::new(center_lat, center_lon)
        .map_err(|_| "projection setup failed".to_owned())?;
    let projected = request
        .radars
        .iter()
        .map(|r| {
            projection
                .forward(r.latitude, r.longitude)
                .map(|xy| (r, xy))
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "radar projection failed".to_owned())?;
    let bounds = projected.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut b, (r, (x, y))| {
            b[0] = b[0].min(x - r.range_m);
            b[1] = b[1].min(y - r.range_m);
            b[2] = b[2].max(x + r.range_m);
            b[3] = b[3].max(y + r.range_m);
            b
        },
    );
    let mut coordinates = request
        .radars
        .iter()
        .map(|r| tiles_for_radius(r.latitude, r.longitude, r.range_m))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "invalid terrain extent".to_owned())?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    coordinates.sort();
    coordinates.dedup();
    let mosaic = s
        .terrain
        .prepare_mosaic(&coordinates)
        .await
        .map_err(|_| "terrain preparation failed".to_owned())?;
    let raster = MetricRaster::from_bounds(
        &mosaic,
        projection,
        bounds,
        request.resolution_m as f64,
        s.max_cells as usize,
    )
    .map_err(|_| "common grid exceeds limits or cannot be projected".to_owned())?;
    let terrain_digest = terrain_hash(&coordinates);
    let shared_elevations: Arc<[Option<f32>]> = raster.elevations_m.clone().into();
    for (index, radar) in request.radars.iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        set_progress(
            s,
            id,
            0.1 + 0.35 * (index as f32 / request.radars.len() as f32),
        )
        .await;
        let radar_clone = radar.clone();
        let k = request.effective_earth_k;
        let width = raster.width;
        let height = raster.height;
        let resolution = raster.resolution_m;
        let projection_name = raster.projection.clone();
        let origin = raster.origin_m;
        let elevations = shared_elevations.clone();
        let (radar_x_m, radar_y_m) = projection
            .forward(radar.latitude, radar.longitude)
            .map_err(|_| "radar projection failed".to_owned())?;
        let radar_x = ((radar_x_m - origin[0]) / resolution).round() as usize;
        let radar_y = ((bounds[3] - radar_y_m) / resolution).round() as usize;
        let coverage = tokio::task::spawn_blocking(move || {
            let grid = Grid::from_shared(width, height, elevations)
                .map_err(|_| "invalid metric terrain grid".to_owned())?;
            compute_coverage(
                &grid,
                &LosConfig {
                    radar_x,
                    radar_y,
                    antenna_agl_m: radar_clone.antenna_agl_m,
                    cell_size_m: resolution,
                    range_m: radar_clone.range_m,
                    effective_earth_k: k,
                    angular_sectors: angular_sectors(radar_clone.range_m, resolution),
                },
            )
            .map_err(|_| "LOS computation failed".to_owned())
        })
        .await
        .map_err(|_| "LOS worker failed".to_owned())??;
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        let config_json =
            serde_json::to_vec(radar).map_err(|_| "radar serialization failed".to_owned())?;
        let config_hash = blake3::hash(&config_json).to_hex().to_string();
        let meta = Metadata {
            radar_id: radar.id.to_string(),
            radar_config_hash: config_hash,
            terrain_hash: terrain_digest.clone(),
            calculated_at: now(),
            crs: projection_name,
            origin,
            resolution_m: resolution,
            extent: bounds,
            width: u32::try_from(width).map_err(|_| "grid width overflow".to_owned())?,
            height: u32::try_from(height).map_err(|_| "grid height overflow".to_owned())?,
            range_m: radar.range_m,
            effective_earth_k: k,
            nodata: u16::MAX,
        };
        let base =
            s.result_directory
                .join(format!("{}-{}", radar.id, &meta.radar_config_hash[..16]));
        write_rcov(
            &base.with_extension("rcov"),
            &meta,
            coverage.ground_visible.words(),
            true,
        )
        .map_err(|_| "cannot persist visibility".to_owned())?;
        write_rhgt(
            &base.with_extension("rhgt"),
            &meta,
            &coverage.minimum_agl_m,
            true,
        )
        .map_err(|_| "cannot persist minimum height".to_owned())?;
        set_progress(
            s,
            id,
            0.45 + 0.5 * ((index + 1) as f32 / request.radars.len() as f32),
        )
        .await;
    }
    Ok(())
}
async fn set_progress(s: &App, id: Uuid, value: f32) {
    if let Some(job) = s.jobs.write().await.get_mut(&id) {
        job.status.progress = value.clamp(0., 1.)
    }
}
fn angular_sectors(range: f64, resolution: f64) -> usize {
    let radius = (range / resolution).ceil();
    ((std::f64::consts::TAU * radius).ceil() as usize).max(8)
}
fn terrain_hash(coordinates: &[terrain_srtm::TileCoordinate]) -> String {
    let mut h = blake3::Hasher::new();
    for c in coordinates {
        h.update(&c.lat.to_le_bytes());
        h.update(&c.lon.to_le_bytes());
    }
    h.finalize().to_hex().to_string()
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
