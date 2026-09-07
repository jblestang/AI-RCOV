use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use coverage_core::{compute_coverage, compute_profile, Grid, LosConfig, LOS_ALGORITHM_VERSION};
use coverage_storage::{
    merge_rhgt_counts_streaming, merge_rhgt_minimum_streaming, read_rdem_value,
    validate as validate_envelope, write_rcov, write_rdem, write_rhgt, Metadata, RCOV_MAGIC,
    RDEM_MAGIC, RHGT_MAGIC,
};
use radar_api::{FusionRequest, FusionResponse, JobRequest, JobState, JobStatus, Radar};
use radar_wmts::{etag, grayscale_png, TILE_SIZE};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use terrain_srtm::{tiles_for_radius, LocalProjection, MetricRaster, SrtmCache, SrtmCacheConfig};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    limit::RequestBodyLimitLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use uuid::Uuid;
const DEFAULT_MAX_RADARS: usize = 32;
const DEFAULT_MAX_CELLS: u64 = 100_000_000;
#[derive(Clone)]
struct JobRecord {
    status: JobStatus,
    cancelled: Arc<AtomicBool>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct JobCacheManifest {
    request_hash: String,
    artifacts: Vec<JobCacheArtifact>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct JobCacheArtifact {
    rcov: String,
    rhgt: String,
    rdem: String,
}

#[derive(Clone)]
struct App {
    radars: Arc<RwLock<HashMap<Uuid, Radar>>>,
    jobs: Arc<RwLock<HashMap<Uuid, JobRecord>>>,
    job_keys: Arc<RwLock<HashMap<String, Uuid>>>,
    job_slots: Arc<Semaphore>,
    fusion_lock: Arc<Mutex<()>>,
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
            job_keys: Default::default(),
            job_slots: Arc::new(Semaphore::new(env_usize("RADAR_MAX_CONCURRENT_JOBS", 2))),
            fusion_lock: Default::default(),
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
        .route("/api/v1/profiles/{id}", get(profile))
        .route(
            "/wmts/{dataset}/{version}/{date}/metadata.json",
            get(wmts_metadata),
        )
        .route(
            "/wmts/{dataset}/{version}/{date}/WMTSCapabilities.xml",
            get(wmts_capabilities),
        )
        .route("/wmts/{dataset}/{version}/{date}/sample", get(wmts_sample))
        .route(
            "/wmts/{dataset}/{version}/{date}/{layer}/{z}/{row}/{tile}",
            get(wmts_tile),
        )
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(cors_layer())
        .layer(PropagateRequestIdLayer::new(request_id_header()))
        .layer(SetRequestIdLayer::new(request_id_header(), MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let addr = std::env::var("RADAR_BIND").unwrap_or_else(|_| "0.0.0.0:8100".into());
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
    let request_key = request_hash(&request)?;
    if let Some(existing_id) = s.job_keys.read().await.get(&request_key).copied() {
        let existing_status = s
            .jobs
            .read()
            .await
            .get(&existing_id)
            .map(|existing| existing.status.clone());
        if let Some(existing) = existing_status {
            let reusable = match existing.state {
                JobState::Queued | JobState::Running => true,
                JobState::Completed => cached_job_valid(s.result_directory.as_ref(), &request_key),
                JobState::Failed | JobState::Cancelled => false,
            };
            if reusable {
                return Ok((StatusCode::ACCEPTED, Json(existing)));
            }
        }
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
    s.job_keys.write().await.insert(request_key, id);
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
    let request_key =
        job_request_hash(request).map_err(|_| "cannot fingerprint coverage request".to_owned())?;
    if cached_job_valid(s.result_directory.as_ref(), &request_key) {
        return Ok(());
    }
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
    let coverage_disks = projected
        .iter()
        .map(|(radar, (x, y))| [*x, *y, radar.range_m])
        .collect::<Vec<_>>();
    let raster = MetricRaster::from_bounds(
        &mosaic,
        projection,
        bounds,
        &coverage_disks,
        request.resolution_m as f64,
        s.max_cells as usize,
    )
    .map_err(|_| "common grid exceeds limits or cannot be projected".to_owned())?;
    let terrain_digest = mosaic.content_hash();
    let shared_elevations: Arc<[Option<f32>]> = raster.elevations_m.clone().into();
    let mut artifacts = Vec::with_capacity(request.radars.len());
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
        let config_json = serde_json::to_vec(&serde_json::json!({
            "los_algorithm_version": LOS_ALGORITHM_VERSION,
            "radar": radar,
            "resolution_m": request.resolution_m,
            "effective_earth_k": request.effective_earth_k,
            "projection": projection_name,
            "origin": origin,
            "extent": bounds,
            "width": width,
            "height": height,
            "terrain_hash": terrain_digest,
        }))
        .map_err(|_| "radar serialization failed".to_owned())?;
        let config_hash = blake3::hash(&config_json).to_hex().to_string();
        let meta = Metadata {
            radar_id: radar.id.to_string(),
            los_algorithm_version: LOS_ALGORITHM_VERSION,
            radar_config_hash: config_hash,
            terrain_hash: terrain_digest.clone(),
            calculated_at: now(),
            crs: projection_name.clone(),
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
        let rcov_path = base.with_extension("rcov");
        let rhgt_path = base.with_extension("rhgt");
        let rdem_path = base.with_extension("rdem");
        let terrain_cached = validate_envelope(&rdem_path, RDEM_MAGIC)
            .is_ok_and(|stored| same_coverage_identity(&stored, &meta));
        if !terrain_cached {
            write_rdem(&rdem_path, &meta, &shared_elevations, true)
                .map_err(|_| "cannot persist terrain elevations".to_owned())?;
        }
        let cached = match (
            validate_envelope(&rcov_path, RCOV_MAGIC),
            validate_envelope(&rhgt_path, RHGT_MAGIC),
        ) {
            (Ok(rcov), Ok(rhgt)) => {
                same_coverage_identity(&rcov, &meta)
                    && same_coverage_identity(&rhgt, &meta)
                    && rcov == rhgt
            }
            _ => false,
        };
        if cached {
            artifacts.push(JobCacheArtifact {
                rcov: file_name(&rcov_path)?,
                rhgt: file_name(&rhgt_path)?,
                rdem: file_name(&rdem_path)?,
            });
            set_progress(
                s,
                id,
                0.45 + 0.5 * ((index + 1) as f32 / request.radars.len() as f32),
            )
            .await;
            continue;
        }
        let radar_clone = radar.clone();
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
                },
            )
            .map_err(|_| "LOS computation failed".to_owned())
        })
        .await
        .map_err(|_| "LOS worker failed".to_owned())??;
        if cancelled.load(Ordering::Acquire) {
            return Ok(());
        }
        write_rcov(&rcov_path, &meta, coverage.ground_visible.words(), true)
            .map_err(|_| "cannot persist visibility".to_owned())?;
        write_rhgt(&rhgt_path, &meta, &coverage.minimum_agl_m, true)
            .map_err(|_| "cannot persist minimum height".to_owned())?;
        artifacts.push(JobCacheArtifact {
            rcov: file_name(&rcov_path)?,
            rhgt: file_name(&rhgt_path)?,
            rdem: file_name(&rdem_path)?,
        });
        set_progress(
            s,
            id,
            0.45 + 0.5 * ((index + 1) as f32 / request.radars.len() as f32),
        )
        .await;
    }
    write_job_cache(
        s.result_directory.as_ref(),
        &JobCacheManifest {
            request_hash: request_key,
            artifacts,
        },
    )?;
    Ok(())
}
async fn set_progress(s: &App, id: Uuid, value: f32) {
    if let Some(job) = s.jobs.write().await.get_mut(&id) {
        job.status.progress = value.clamp(0., 1.)
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
#[derive(serde::Deserialize)]
struct ProfileQuery {
    latitude: f64,
    longitude: f64,
    target_agl_m: u16,
}
async fn profile(
    State(s): State<App>,
    Path(id): Path<Uuid>,
    Query(query): Query<ProfileQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let radar = s
        .radars
        .read()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| public_error(StatusCode::NOT_FOUND, "radar not found"))?;
    if !(-90.0..=90.0).contains(&query.latitude) || !(-180.0..=180.0).contains(&query.longitude) {
        return Err(public_error(StatusCode::BAD_REQUEST, "target coordinates"));
    }
    let projection = LocalProjection::new(radar.latitude, radar.longitude)
        .map_err(|_| public_error(StatusCode::BAD_REQUEST, "projection"))?;
    let (tx, ty) = projection
        .forward(query.latitude, query.longitude)
        .map_err(|_| public_error(StatusCode::BAD_REQUEST, "projection"))?;
    let distance = tx.hypot(ty);
    if distance > radar.range_m {
        return Err(public_error(
            StatusCode::BAD_REQUEST,
            "target outside radar range",
        ));
    }
    let coordinates = tiles_for_radius(radar.latitude, radar.longitude, radar.range_m)
        .map_err(|_| public_error(StatusCode::BAD_REQUEST, "terrain extent"))?;
    let mosaic = s.terrain.prepare_mosaic(&coordinates).await.map_err(|_| {
        public_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "terrain preparation failed",
        )
    })?;
    let step_m = env_u64("RADAR_PROFILE_STEP_M", 90) as f64;
    let steps = (distance / step_m).ceil().max(1.) as usize;
    let mut elevations = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let f = step as f64 / steps as f64;
        let (lat, lon) = projection.inverse(tx * f, ty * f).map_err(|_| {
            public_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "profile projection failed",
            )
        })?;
        elevations.push(
            mosaic
                .sample(lat, lon)
                .map_err(|_| {
                    public_error(StatusCode::SERVICE_UNAVAILABLE, "profile terrain missing")
                })?
                .map(f32::from),
        )
    }
    let grid = Grid::new(steps + 1, 1, elevations)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "profile grid failed"))?;
    let config = LosConfig {
        radar_x: 0,
        radar_y: 0,
        antenna_agl_m: radar.antenna_agl_m,
        cell_size_m: distance / steps as f64,
        range_m: distance,
        effective_earth_k: 4. / 3.,
    };
    let result =
        compute_profile(&grid, &config, steps, 0, query.target_agl_m as f64).map_err(|_| {
            public_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "profile computation failed",
            )
        })?;
    let points=result.points.into_iter().map(|p|serde_json::json!({"distance_m":p.distance_m,"terrain_m":p.terrain_m,"apparent_terrain_m":p.apparent_terrain_m,"los_height_m":p.los_height_m,"horizon_slope":p.horizon_slope,"target_height_m":p.target_height_m,"is_obstruction":p.is_obstruction,"vertical_margin_m":p.vertical_margin_m})).collect::<Vec<_>>();
    Ok(Json(
        serde_json::json!({"radar_id":id,"target":{"latitude":query.latitude,"longitude":query.longitude,"agl_m":query.target_agl_m},"visible":result.visible,"first_obstacle_distance_m":result.first_obstacle_distance_m,"points":points}),
    ))
}
async fn fusion(
    State(s): State<App>,
    Json(req): Json<FusionRequest>,
) -> ApiResult<(StatusCode, Json<FusionResponse>)> {
    let _fusion_guard = s.fusion_lock.lock().await;
    if req.radar_ids.len() > s.max_radars {
        return Err(public_error(StatusCode::BAD_REQUEST, "radar count"));
    }
    let mut unique = req.radar_ids.clone();
    unique.sort();
    unique.dedup();
    if unique.len() != req.radar_ids.len() {
        return Err(public_error(StatusCode::BAD_REQUEST, "duplicate radar id"));
    }
    let mut paths = Vec::with_capacity(unique.len());
    for id in &unique {
        paths.push(
            latest_rhgt(s.result_directory.as_ref(), *id)
                .map_err(|_| public_error(StatusCode::NOT_FOUND, "coverage not found"))?,
        )
    }
    let signature = fusion_signature(&paths, req.target_agl_m)?;
    let (terrain_metadata, terrain_file) = if let Some(path) = paths.first() {
        let terrain_path = path.with_extension("rdem");
        let metadata = validate_envelope(&terrain_path, RDEM_MAGIC).map_err(|_| {
            public_error(StatusCode::UNPROCESSABLE_ENTITY, "terrain coverage missing")
        })?;
        let name = file_name(&terrain_path)
            .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "terrain path failed"))?;
        (Some(metadata), Some(name))
    } else {
        (None, None)
    };
    let fusion_id = uuid_from_hash(&signature);
    let directory = s.result_directory.clone();
    let target = req.target_agl_m;
    let id_for_worker = fusion_id;
    let dataset = directory.join("datasets").join(fusion_id.to_string());
    if let Some(response) = cached_fusion_response(&dataset, fusion_id, &req)? {
        return Ok((StatusCode::OK, Json(response)));
    }
    let (metadata, layers, minimum) = tokio::task::spawn_blocking(move || {
        let mut layers = Vec::new();
        let mut meta = None;
        for height in [0, 30, 50, 100, target] {
            if layers.iter().any(|(h, _)| *h == height) {
                continue;
            }
            let (m, c) = merge_rhgt_counts_streaming(&paths, height)?;
            if meta.is_none() {
                meta = m
            }
            layers.push((height, c));
        }
        let (_, minimum) = merge_rhgt_minimum_streaming(&paths)?;
        Ok::<_, coverage_storage::StorageError>((meta, layers, minimum))
    })
    .await
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "fusion worker failed"))?
    .map_err(|_| {
        public_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "coverage layers are incompatible",
        )
    })?;
    let (width, height) = metadata.as_ref().map_or((0, 0), |m| (m.width, m.height));
    std::fs::create_dir_all(&dataset)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed"))?;
    for (height, counts) in &layers {
        let name = match *height {
            0 => "ground.bin".into(),
            30 => "agl-30m.bin".into(),
            50 => "agl-50m.bin".into(),
            100 => "agl-100m.bin".into(),
            h => format!("agl-{h}m.bin"),
        };
        let boolean = counts
            .iter()
            .map(|v| if *v > 0 { 255 } else { 0 })
            .collect::<Vec<_>>();
        atomic_bytes(&dataset.join(name), &boolean).map_err(|_| {
            public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed")
        })?;
        if *height == target {
            atomic_bytes(&dataset.join("radar-count.bin"), counts).map_err(|_| {
                public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed")
            })?
        }
    }
    let minimum_bytes = minimum
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<_>>();
    atomic_bytes(&dataset.join("min-detection-height.bin"), &minimum_bytes)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed"))?;
    if metadata.as_ref() != terrain_metadata.as_ref() {
        return Err(public_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "terrain grid is incompatible",
        ));
    }
    let manifest = serde_json::json!({"dataset_id":id_for_worker,"version":1,"date":Utc::now().format("%Y-%m-%d").to_string(),"target_agl_m":target,"radar_ids":unique,"metadata":metadata,"terrain_rdem":terrain_file});
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "manifest failed"))?;
    atomic_bytes(&dataset.join("metadata.json"), &manifest_bytes)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed"))?;
    Ok((
        StatusCode::CREATED,
        Json(FusionResponse {
            fusion_id,
            selected_radars: req.radar_ids,
            target_agl_m: target,
            width,
            height,
            dataset_url: format!(
                "/wmts/{fusion_id}/1/{}/metadata.json",
                Utc::now().format("%Y-%m-%d")
            ),
        }),
    ))
}
fn latest_rhgt(
    directory: &std::path::Path,
    id: Uuid,
) -> Result<std::path::PathBuf, std::io::Error> {
    let prefix = format!("{id}-");
    let mut candidates = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|v| v == "rhgt")
                && p.file_name()
                    .and_then(|v| v.to_str())
                    .is_some_and(|v| v.starts_with(&prefix))
        })
        .filter_map(|p| {
            std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .map(|t| (t, p))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|v| v.0);
    candidates
        .pop()
        .map(|v| v.1)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "coverage"))
}

fn request_hash(request: &JobRequest) -> ApiResult<String> {
    job_request_hash(request)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "job fingerprint failed"))
}

fn job_request_hash(request: &JobRequest) -> Result<String, serde_json::Error> {
    serde_json::to_vec(request).map(|bytes| {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"radial-los-job");
        hasher.update(&LOS_ALGORITHM_VERSION.to_le_bytes());
        hasher.update(&bytes);
        hasher.finalize().to_hex().to_string()
    })
}

fn job_cache_path(directory: &std::path::Path, request_hash: &str) -> std::path::PathBuf {
    directory.join("jobs").join(format!("{request_hash}.json"))
}

fn cached_job_valid(directory: &std::path::Path, request_hash: &str) -> bool {
    let Ok(bytes) = std::fs::read(job_cache_path(directory, request_hash)) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<JobCacheManifest>(&bytes) else {
        return false;
    };
    if manifest.request_hash != request_hash || manifest.artifacts.is_empty() {
        return false;
    }
    manifest.artifacts.iter().all(|artifact| {
        let rcov = directory.join(&artifact.rcov);
        let rhgt = directory.join(&artifact.rhgt);
        let rdem = directory.join(&artifact.rdem);
        if rcov.parent() != Some(directory)
            || rhgt.parent() != Some(directory)
            || rdem.parent() != Some(directory)
            || rcov.file_name().and_then(|v| v.to_str()) != Some(artifact.rcov.as_str())
            || rhgt.file_name().and_then(|v| v.to_str()) != Some(artifact.rhgt.as_str())
            || rdem.file_name().and_then(|v| v.to_str()) != Some(artifact.rdem.as_str())
        {
            return false;
        }
        match (
            validate_envelope(&rcov, RCOV_MAGIC),
            validate_envelope(&rhgt, RHGT_MAGIC),
            validate_envelope(&rdem, RDEM_MAGIC),
        ) {
            (Ok(a), Ok(b), Ok(c)) => a == b && b == c,
            _ => false,
        }
    })
}

fn write_job_cache(directory: &std::path::Path, manifest: &JobCacheManifest) -> Result<(), String> {
    let path = job_cache_path(directory, &manifest.request_hash);
    let parent = path
        .parent()
        .ok_or_else(|| "invalid job cache path".to_owned())?;
    std::fs::create_dir_all(parent).map_err(|_| "cannot prepare job cache".to_owned())?;
    let bytes =
        serde_json::to_vec_pretty(manifest).map_err(|_| "cannot serialize job cache".to_owned())?;
    atomic_bytes(&path, &bytes).map_err(|_| "cannot persist job cache".to_owned())
}

fn file_name(path: &std::path::Path) -> Result<String, String> {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
        .ok_or_else(|| "invalid result filename".to_owned())
}

fn same_coverage_identity(a: &Metadata, b: &Metadata) -> bool {
    a.radar_id == b.radar_id
        && a.los_algorithm_version == b.los_algorithm_version
        && a.radar_config_hash == b.radar_config_hash
        && a.terrain_hash == b.terrain_hash
        && a.crs == b.crs
        && a.origin == b.origin
        && a.resolution_m == b.resolution_m
        && a.extent == b.extent
        && a.width == b.width
        && a.height == b.height
        && a.range_m == b.range_m
        && a.effective_earth_k == b.effective_earth_k
        && a.nodata == b.nodata
}

fn fusion_signature(paths: &[std::path::PathBuf], target: u16) -> ApiResult<blake3::Hash> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"radial-fusion-v1");
    hasher.update(&target.to_le_bytes());
    for path in paths {
        let metadata = validate_envelope(path, RHGT_MAGIC)
            .map_err(|_| public_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid coverage"))?;
        let encoded = serde_json::to_vec(&metadata).map_err(|_| {
            public_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "fusion fingerprint failed",
            )
        })?;
        hasher.update(&(encoded.len() as u64).to_le_bytes());
        hasher.update(&encoded);
    }
    Ok(hasher.finalize())
}

fn uuid_from_hash(hash: &blake3::Hash) -> Uuid {
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn cached_fusion_response(
    dataset: &std::path::Path,
    fusion_id: Uuid,
    req: &FusionRequest,
) -> ApiResult<Option<FusionResponse>> {
    let manifest_path = dataset.join("metadata.json");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&manifest_path)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset cache failed"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid dataset cache"))?;
    if !req.radar_ids.is_empty() {
        let Some(terrain_file) = manifest["terrain_rdem"].as_str() else {
            return Ok(None);
        };
        if std::path::Path::new(terrain_file)
            .file_name()
            .and_then(|value| value.to_str())
            != Some(terrain_file)
            || !dataset
                .parent()
                .and_then(std::path::Path::parent)
                .is_some_and(|results| results.join(terrain_file).is_file())
        {
            return Ok(None);
        }
    }
    let required = [
        "ground.bin".to_owned(),
        "agl-30m.bin".to_owned(),
        "agl-50m.bin".to_owned(),
        "agl-100m.bin".to_owned(),
        format!("agl-{}m.bin", req.target_agl_m),
        "radar-count.bin".to_owned(),
        "min-detection-height.bin".to_owned(),
    ];
    if required.iter().any(|name| !dataset.join(name).is_file()) {
        return Ok(None);
    }
    let width = manifest["metadata"]["width"].as_u64().unwrap_or(0) as u32;
    let height = manifest["metadata"]["height"].as_u64().unwrap_or(0) as u32;
    let date = manifest["date"]
        .as_str()
        .ok_or_else(|| public_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid dataset cache"))?;
    Ok(Some(FusionResponse {
        fusion_id,
        selected_radars: req.radar_ids.clone(),
        target_agl_m: req.target_agl_m,
        width,
        height,
        dataset_url: format!("/wmts/{fusion_id}/1/{date}/metadata.json"),
    }))
}
fn atomic_bytes(path: &std::path::Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.flush()?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(tmp, path)
}
async fn wmts_metadata(
    State(s): State<App>,
    Path((dataset, version, date)): Path<(Uuid, u16, String)>,
) -> ApiResult<Response> {
    let dir = dataset_directory(&s, dataset, version, &date)?;
    let bytes = std::fs::read(dir.join("metadata.json"))
        .map_err(|_| public_error(StatusCode::NOT_FOUND, "dataset not found"))?;
    response_bytes(StatusCode::OK, "application/json", bytes, None)
}
async fn wmts_capabilities(
    State(s): State<App>,
    Path((dataset, version, date)): Path<(Uuid, u16, String)>,
) -> ApiResult<Response> {
    let dir = dataset_directory(&s, dataset, version, &date)?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("metadata.json"))
            .map_err(|_| public_error(StatusCode::NOT_FOUND, "dataset not found"))?,
    )
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid dataset"))?;
    let meta = &manifest["metadata"];
    let width = meta["width"].as_u64().unwrap_or(0) as usize;
    let height = meta["height"].as_u64().unwrap_or(0) as usize;
    let levels = tile_levels(width, height);
    let mut matrices = String::new();
    for z in 0..levels {
        let factor = 1usize << (levels - 1 - z);
        matrices.push_str(&format!("<TileMatrix><ows:Identifier>{z}</ows:Identifier><ScaleDenominator>{}</ScaleDenominator><TopLeftCorner>{} {}</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight><MatrixWidth>{}</MatrixWidth><MatrixHeight>{}</MatrixHeight></TileMatrix>",meta["resolution_m"].as_f64().unwrap_or(90.)*factor as f64/0.00028,meta["extent"][0],meta["extent"][3],width.div_ceil(TILE_SIZE*factor),height.div_ceil(TILE_SIZE*factor)));
    }
    let mut layers = String::new();
    for (name, title) in [
        ("ground", "Ground visibility"),
        ("agl-30m", "Visibility at 30 m AGL"),
        ("agl-50m", "Visibility at 50 m AGL"),
        ("agl-100m", "Visibility at 100 m AGL"),
        ("min-detection-height", "Minimum detection height"),
        ("radar-count", "Radar count"),
    ] {
        let template = format!(
            "/wmts/{dataset}/{version}/{date}/{name}/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"
        );
        layers.push_str(&format!("<Layer><ows:Title>{title}</ows:Title><ows:Identifier>{name}</ows:Identifier><Format>image/png</Format><ResourceURL format=\"image/png\" resourceType=\"tile\" template=\"{template}\"/><TileMatrixSetLink><TileMatrixSet>radial</TileMatrixSet></TileMatrixSetLink></Layer>"));
    }
    let crs = xml_escape(meta["crs"].as_str().unwrap_or("local metric"));
    let xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Capabilities xmlns=\"http://www.opengis.net/wmts/1.0\" xmlns:ows=\"http://www.opengis.net/ows/1.1\" version=\"1.0.0\"><Contents>{layers}<TileMatrixSet><ows:Identifier>radial</ows:Identifier><ows:SupportedCRS>{crs}</ows:SupportedCRS>{matrices}</TileMatrixSet></Contents></Capabilities>");
    response_bytes(StatusCode::OK, "application/xml", xml.into_bytes(), None)
}

#[derive(serde::Deserialize)]
struct SampleQuery {
    col: u32,
    row: u32,
}

async fn wmts_sample(
    State(s): State<App>,
    Path((dataset, version, date)): Path<(Uuid, u16, String)>,
    Query(query): Query<SampleQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let dir = dataset_directory(&s, dataset, version, &date)?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("metadata.json"))
            .map_err(|_| public_error(StatusCode::NOT_FOUND, "dataset not found"))?,
    )
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid dataset"))?;
    let width = manifest["metadata"]["width"].as_u64().unwrap_or(0) as usize;
    let height = manifest["metadata"]["height"].as_u64().unwrap_or(0) as usize;
    let col = query.col as usize;
    let row = query.row as usize;
    if col >= width || row >= height {
        return Err(public_error(StatusCode::NOT_FOUND, "sample outside grid"));
    }
    let index = row * width + col;
    let terrain_file = manifest["terrain_rdem"]
        .as_str()
        .ok_or_else(|| public_error(StatusCode::NOT_FOUND, "terrain samples unavailable"))?;
    if std::path::Path::new(terrain_file)
        .file_name()
        .and_then(|v| v.to_str())
        != Some(terrain_file)
    {
        return Err(public_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid terrain path",
        ));
    }
    let terrain_path = s.result_directory.join(terrain_file);
    let minimum_path = dir.join("min-detection-height.bin");
    let (terrain, minimum) = tokio::task::spawn_blocking(move || {
        let terrain = read_rdem_value(&terrain_path, index)?;
        let minimum = read_u16_value(&minimum_path, index)?;
        Ok::<_, coverage_storage::StorageError>((terrain, minimum))
    })
    .await
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "sample worker failed"))?
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "sample read failed"))?;
    Ok(Json(serde_json::json!({
        "col": col,
        "row": row,
        "terrain_elevation_amsl_m": terrain,
        "minimum_detection_agl_m": minimum.filter(|value| *value != u16::MAX),
    })))
}

fn read_u16_value(
    path: &std::path::Path,
    index: usize,
) -> Result<Option<u16>, coverage_storage::StorageError> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let offset = index
        .checked_mul(2)
        .ok_or(coverage_storage::StorageError::Format("sample index"))?;
    file.seek(SeekFrom::Start(offset as u64))?;
    let mut bytes = [0u8; 2];
    file.read_exact(&mut bytes)?;
    Ok(Some(u16::from_le_bytes(bytes)))
}

fn xml_escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
async fn wmts_tile(
    State(s): State<App>,
    Path((dataset, version, date, layer, z, row, tile)): Path<(
        Uuid,
        u16,
        String,
        String,
        u8,
        u32,
        String,
    )>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let col = tile
        .strip_suffix(".png")
        .ok_or_else(|| public_error(StatusCode::BAD_REQUEST, "tile suffix"))?
        .parse::<u32>()
        .map_err(|_| public_error(StatusCode::BAD_REQUEST, "tile column"))?;
    let dir = dataset_directory(&s, dataset, version, &date)?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("metadata.json"))
            .map_err(|_| public_error(StatusCode::NOT_FOUND, "dataset not found"))?,
    )
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid dataset"))?;
    let metadata = &manifest["metadata"];
    let width = metadata["width"].as_u64().unwrap_or(0) as usize;
    let height = metadata["height"].as_u64().unwrap_or(0) as usize;
    if width == 0 || height == 0 {
        return Err(public_error(StatusCode::NO_CONTENT, "empty dataset"));
    }
    let levels = tile_levels(width, height);
    if z >= levels {
        return Err(public_error(StatusCode::BAD_REQUEST, "tile matrix"));
    }
    let factor = 1usize << (levels - 1 - z);
    let matrix_width = width.div_ceil(TILE_SIZE * factor);
    let matrix_height = height.div_ceil(TILE_SIZE * factor);
    if col as usize >= matrix_width || row as usize >= matrix_height {
        return Err(public_error(StatusCode::NOT_FOUND, "tile outside matrix"));
    }
    let source = layer_source(&layer)?;
    let cache = dir
        .join("tiles")
        .join(&layer)
        .join(z.to_string())
        .join(row.to_string())
        .join(format!("{col}.png"));
    let bytes = if cache.exists() {
        std::fs::read(&cache)
            .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "tile cache failed"))?
    } else {
        let path = dir.join(source);
        let minimum = layer == "min-detection-height";
        let generated = tokio::task::spawn_blocking(move || {
            if minimum {
                render_minimum_tile(&path, width, height, factor, row as usize, col as usize)
            } else {
                render_count_tile(&path, width, height, factor, row as usize, col as usize)
            }
        })
        .await
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "tile worker failed"))?
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "tile generation failed"))?;
        if let Some(parent) = cache.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "tile cache failed"))?
        }
        atomic_bytes(&cache, &generated)
            .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "tile cache failed"))?;
        generated
    };
    let tag = etag(&bytes);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(tag.as_str())
    {
        return response_bytes(StatusCode::NOT_MODIFIED, "image/png", Vec::new(), Some(tag));
    }
    response_bytes(StatusCode::OK, "image/png", bytes, Some(tag))
}
fn layer_source(layer: &str) -> ApiResult<String> {
    let fixed = match layer {
        "radar-count" => Some("radar-count.bin"),
        "ground" => Some("ground.bin"),
        "agl-30m" => Some("agl-30m.bin"),
        "agl-50m" => Some("agl-50m.bin"),
        "agl-100m" => Some("agl-100m.bin"),
        "min-detection-height" => Some("min-detection-height.bin"),
        _ => None,
    };
    if let Some(name) = fixed {
        return Ok(name.into());
    }
    let height = layer
        .strip_prefix("agl-")
        .and_then(|v| v.strip_suffix('m'))
        .and_then(|v| v.parse::<u16>().ok())
        .ok_or_else(|| public_error(StatusCode::BAD_REQUEST, "layer"))?;
    Ok(format!("agl-{height}m.bin"))
}
fn dataset_directory(s: &App, id: Uuid, version: u16, date: &str) -> ApiResult<std::path::PathBuf> {
    if version != 1
        || date.len() != 10
        || !date.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err(public_error(
            StatusCode::BAD_REQUEST,
            "dataset version or date",
        ));
    }
    let dir = s.result_directory.join("datasets").join(id.to_string());
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.join("metadata.json"))
            .map_err(|_| public_error(StatusCode::NOT_FOUND, "dataset not found"))?,
    )
    .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "invalid dataset"))?;
    if manifest["version"] != version || manifest["date"] != date {
        return Err(public_error(
            StatusCode::NOT_FOUND,
            "dataset version not found",
        ));
    }
    Ok(dir)
}
fn tile_levels(width: usize, height: usize) -> u8 {
    let mut size = width.max(height);
    let mut levels = 1;
    while size > TILE_SIZE {
        size = size.div_ceil(2);
        levels += 1
    }
    levels
}
fn render_count_tile(
    path: &std::path::Path,
    width: usize,
    height: usize,
    factor: usize,
    row: usize,
    col: usize,
) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let mut output = vec![0u8; TILE_SIZE * TILE_SIZE];
    let tile_source_x = col * TILE_SIZE * factor;
    let tile_source_y = row * TILE_SIZE * factor;
    let source_width = (TILE_SIZE * factor).min(width.saturating_sub(tile_source_x));
    let mut line = vec![0u8; source_width];
    for ty in 0..TILE_SIZE {
        let source_y = tile_source_y + ty * factor;
        if source_y >= height || source_width == 0 {
            break;
        }
        for sy in source_y..(source_y + factor).min(height) {
            file.seek(SeekFrom::Start((sy * width + tile_source_x) as u64))?;
            file.read_exact(&mut line)?;
            for (tx, values) in line.chunks(factor).enumerate() {
                output[ty * TILE_SIZE + tx] =
                    output[ty * TILE_SIZE + tx].max(values.iter().copied().max().unwrap_or(0));
            }
        }
    }
    grayscale_png(&output).map_err(std::io::Error::other)
}
fn render_minimum_tile(
    path: &std::path::Path,
    width: usize,
    height: usize,
    factor: usize,
    row: usize,
    col: usize,
) -> Result<Vec<u8>, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let mut output = vec![0u8; TILE_SIZE * TILE_SIZE];
    let tile_source_x = col * TILE_SIZE * factor;
    let tile_source_y = row * TILE_SIZE * factor;
    let source_width = (TILE_SIZE * factor).min(width.saturating_sub(tile_source_x));
    let mut line = vec![0u8; source_width * 2];
    let mut minimum = vec![u16::MAX; TILE_SIZE];
    for ty in 0..TILE_SIZE {
        let source_y = tile_source_y + ty * factor;
        if source_y >= height || source_width == 0 {
            break;
        }
        minimum.fill(u16::MAX);
        for y in source_y..(source_y + factor).min(height) {
            file.seek(SeekFrom::Start(((y * width + tile_source_x) * 2) as u64))?;
            file.read_exact(&mut line)?;
            for (tx, values) in line.as_chunks::<2>().0.chunks(factor).enumerate() {
                for pair in values {
                    let value = u16::from_le_bytes(*pair);
                    if value != u16::MAX {
                        minimum[tx] = minimum[tx].min(value);
                    }
                }
            }
        }
        for (tx, value) in minimum.iter().enumerate() {
            if *value != u16::MAX {
                output[ty * TILE_SIZE + tx] = (*value / 257) as u8;
            }
        }
    }
    grayscale_png(&output).map_err(std::io::Error::other)
}
fn response_bytes(
    status: StatusCode,
    content_type: &'static str,
    body: Vec<u8>,
    tag: Option<String>,
) -> ApiResult<Response> {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
    if let Some(tag) = tag {
        builder = builder.header(header::ETAG, tag)
    }
    builder
        .body(axum::body::Body::from(body))
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "response failed"))
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
fn request_id_header() -> HeaderName {
    HeaderName::from_static("x-request-id")
}
fn cors_layer() -> CorsLayer {
    let configured = std::env::var("RADAR_CORS_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:8100,http://localhost:5173".into());
    let origins = configured
        .split(',')
        .filter_map(|v| v.trim().parse::<HeaderValue>().ok())
        .collect::<Vec<_>>();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([header::CONTENT_TYPE, request_id_header()])
        .expose_headers([header::ETAG, request_id_header()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn request() -> JobRequest {
        JobRequest {
            radars: vec![Radar {
                id: Uuid::nil(),
                name: "cache test".into(),
                latitude: 45.2,
                longitude: 2.2,
                antenna_agl_m: 20.0,
                range_m: 100_000.0,
                active: true,
            }],
            resolution_m: 30,
            effective_earth_k: 4.0 / 3.0,
            target_heights_agl_m: vec![30, 50, 100],
        }
    }

    #[test]
    fn job_fingerprint_is_stable_and_parameter_sensitive() {
        let original = request();
        assert_eq!(
            request_hash(&original).unwrap(),
            request_hash(&original).unwrap()
        );
        let mut changed = original;
        changed.resolution_m = 90;
        assert_ne!(
            request_hash(&changed).unwrap(),
            request_hash(&request()).unwrap()
        );
    }

    #[test]
    fn fusion_uuid_is_stable() {
        let hash = blake3::hash(b"same fusion inputs");
        assert_eq!(uuid_from_hash(&hash), uuid_from_hash(&hash));
        assert_ne!(
            uuid_from_hash(&hash),
            uuid_from_hash(&blake3::hash(b"different fusion inputs"))
        );
    }

    #[test]
    fn lod_handles_odd_dimensions() {
        assert_eq!(tile_levels(256, 256), 1);
        assert_eq!(tile_levels(257, 255), 2);
        assert_eq!(tile_levels(8891, 8891), 7)
    }
    #[test]
    fn count_tile_has_png_signature() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("radial-count-{}-{nonce}.bin", std::process::id()));
        std::fs::write(&path, [0, 1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let png = render_count_tile(&path, 3, 3, 2, 0, 0).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        std::fs::remove_file(path).unwrap()
    }
    #[test]
    fn xml_values_are_escaped() {
        assert_eq!(xml_escape("a&<\"'"), "a&amp;&lt;&quot;&apos;")
    }
    #[test]
    fn custom_agl_layer_is_safe() {
        assert_eq!(layer_source("agl-75m").unwrap(), "agl-75m.bin");
        assert!(layer_source("agl-../x").is_err());
        assert!(layer_source("agl-70000m").is_err());
    }
}
