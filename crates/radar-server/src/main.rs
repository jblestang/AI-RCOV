use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use coverage_core::{compute_coverage, Grid, LosConfig};
use coverage_storage::{merge_rhgt_counts_streaming, write_rcov, write_rhgt, Metadata};
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
        .route(
            "/wmts/{dataset}/{version}/{date}/metadata.json",
            get(wmts_metadata),
        )
        .route(
            "/wmts/{dataset}/{version}/{date}/WMTSCapabilities.xml",
            get(wmts_capabilities),
        )
        .route(
            "/wmts/{dataset}/{version}/{date}/radar-count/{z}/{row}/{col}.png",
            get(wmts_count_tile),
        )
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
async fn fusion(
    State(s): State<App>,
    Json(req): Json<FusionRequest>,
) -> ApiResult<(StatusCode, Json<FusionResponse>)> {
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
    let directory = s.result_directory.clone();
    let target = req.target_agl_m;
    let fusion_id = Uuid::new_v4();
    let id_for_worker = fusion_id;
    let (metadata, counts) =
        tokio::task::spawn_blocking(move || merge_rhgt_counts_streaming(&paths, target))
            .await
            .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "fusion worker failed"))?
            .map_err(|_| {
                public_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "coverage layers are incompatible",
                )
            })?;
    let (width, height) = metadata.as_ref().map_or((0, 0), |m| (m.width, m.height));
    let dataset = directory.join("datasets").join(fusion_id.to_string());
    std::fs::create_dir_all(&dataset)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed"))?;
    atomic_bytes(&dataset.join("radar-count.bin"), &counts)
        .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "dataset storage failed"))?;
    let manifest = serde_json::json!({"dataset_id":id_for_worker,"version":1,"date":Utc::now().format("%Y-%m-%d").to_string(),"target_agl_m":target,"radar_ids":unique,"metadata":metadata});
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
    let template = format!(
        "/wmts/{dataset}/{version}/{date}/radar-count/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"
    );
    let crs = xml_escape(meta["crs"].as_str().unwrap_or("local metric"));
    let xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Capabilities xmlns=\"http://www.opengis.net/wmts/1.0\" xmlns:ows=\"http://www.opengis.net/ows/1.1\" version=\"1.0.0\"><Contents><Layer><ows:Title>Radar count</ows:Title><ows:Identifier>radar-count</ows:Identifier><Format>image/png</Format><ResourceURL format=\"image/png\" resourceType=\"tile\" template=\"{template}\"/><TileMatrixSetLink><TileMatrixSet>radial</TileMatrixSet></TileMatrixSetLink></Layer><TileMatrixSet><ows:Identifier>radial</ows:Identifier><ows:SupportedCRS>{crs}</ows:SupportedCRS>{matrices}</TileMatrixSet></Contents></Capabilities>");
    response_bytes(StatusCode::OK, "application/xml", xml.into_bytes(), None)
}
fn xml_escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
async fn wmts_count_tile(
    State(s): State<App>,
    Path((dataset, version, date, z, row, col)): Path<(Uuid, u16, String, u8, u32, u32)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
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
    let cache = dir
        .join("tiles")
        .join("radar-count")
        .join(z.to_string())
        .join(row.to_string())
        .join(format!("{col}.png"));
    let bytes = if cache.exists() {
        std::fs::read(&cache)
            .map_err(|_| public_error(StatusCode::INTERNAL_SERVER_ERROR, "tile cache failed"))?
    } else {
        let path = dir.join("radar-count.bin");
        let generated = tokio::task::spawn_blocking(move || {
            render_count_tile(&path, width, height, factor, row as usize, col as usize)
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
    for ty in 0..TILE_SIZE {
        for tx in 0..TILE_SIZE {
            let source_x = (col * TILE_SIZE + tx) * factor;
            let source_y = (row * TILE_SIZE + ty) * factor;
            if source_x >= width || source_y >= height {
                continue;
            }
            let mut max = 0;
            for sy in source_y..(source_y + factor).min(height) {
                let offset = sy * width + source_x;
                file.seek(SeekFrom::Start(offset as u64))?;
                let mut line = vec![0u8; (source_x + factor).min(width) - source_x];
                file.read_exact(&mut line)?;
                max = max.max(line.into_iter().max().unwrap_or(0));
            }
            output[ty * TILE_SIZE + tx] = max
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
