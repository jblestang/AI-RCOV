# Acceptance status

This first repository revision is deliberately honest about incomplete work.

Implemented: network-free LOS core, `k` correction, minimum AGL, unique clipped
rings, angular lookup/exact fallback, bitsets and saturating fusion, profiles,
strict HGT decoding, immutable `Arc<HgtTile>` mosaic, binary envelopes with
atomic writes/checksum, API DTOs, health/readiness and radar CRUD shell,
semantic LOD/PNG/ETag primitives, offline benchmark, container and CI skeleton.

The SRTM layer now has a bounded downloader with a hard-coded HTTPS origin,
disabled redirects, timeouts, bounded retries/concurrency and response-size
limits. It atomically stores compressed tiles, shares complete decoded tiles as
`Arc<HgtTile>`, and builds immutable mosaics. Persistent `.rhgt` fusion now
validates grids and checksums while reading one 64 KiB block per layer; empty
active selections and saturating counts are supported. Format tests cover
round-trips, lookup by configuration hash, corruption, truncation and mismatches.
Geodesic range preparation enumerates every intersecting one-degree tile and
handles antimeridian crossings. The optional native Rayon feature uses a bounded
pool and limits simultaneous per-radar workspaces by an explicit memory budget;
tests assert byte-identical sequential and parallel outputs.
The HTTP layer now validates job resolution, radar count, coordinates, grid-cell
limits and estimated memory before returning `202 Accepted`. Jobs expose stable
IDs, queued/running/failed/cancelled states, progress and timestamps, use a
bounded semaphore, and can be cancelled. Radar updates preserve the path ID.
The native job worker is connected end to end for per-radar artifacts: it
enumerates and downloads all SRTM tiles, builds an immutable mosaic, projects a
metric grid, executes LOS in a blocking native worker, and atomically persists
both `.rcov` and `.rhgt`. A job reaches `completed` only after both validated
artifacts exist for every requested radar; cancellation is checked between
expensive stages.
Multi-radar jobs now plan one shared azimuthal-equidistant grid covering the
union of radar ranges. Terrain samples are allocated once and shared immutably
between LOS workers; all persisted layers therefore have compatible projection,
origin, extent and dimensions for later selection/fusion.
`POST /api/v1/fusions` now resolves selected radar UUIDs to server-owned result
paths, rejects duplicates, streams `.rhgt` layers at the requested integer AGL,
and atomically creates an immutable, versioned/date-stamped dataset manifest and
radar-count payload. Changing the active selection never reruns LOS.
The REST tile service now publishes fusion metadata and 256 px radar-count PNGs
at computed detail levels. It validates UUID/version/date/matrix/row/column,
uses max aggregation, reads only required source rows, emits deterministic ETags,
honours `If-None-Match` with 304, and sets one-year immutable cache headers.
It also emits a WMTS 1.0 GetCapabilities document containing the layer, format,
local metric CRS, matrix dimensions, scales and REST template. Generated PNGs
are atomically cached by dataset/layer/matrix/row/column.
Fusion datasets now materialize ground, 30 m, 50 m, 100 m, arbitrary requested
AGL, radar-count, and best minimum-detection-height products directly from saved
`.rhgt` files. Boolean/count LOD uses any/max; minimum-height LOD uses min while
ignoring NoData. GetCapabilities advertises all six standard layers.
`GET /api/v1/profiles/{radar_id}` builds a server-side terrain transect and
returns cumulative distance, raw/apparent terrain, LOS line, horizon, requested
target height, obstruction flags, vertical margin, first obstacle and final
visibility. Targets outside radar range are rejected. HTTP CORS now uses an
explicit `RADAR_CORS_ORIGINS` allow-list, and UUID correlation IDs are created,
traced and propagated in responses.
The web crate now has a real `wasm-bindgen` entry point, honours compile-time or
runtime API origin configuration, checks server health, loads and safely renders
the server radar list, reports errors, and exposes resolution/height/layer
controls. It contains no terrain or LOS dependency. CI compiles the wasm target.
Separate workflows package/deploy it to GitHub Pages and publish non-root
multi-architecture server images to GHCR.
Terrain identity now hashes coordinates, dimensions and every decoded elevation
sample in stable tile order. Regression tests also cover unknown format versions,
incorrect magic, odd LOD dimensions, XML escaping and PNG signatures.
The dependency-free standalone benchmark now imports the production LOS modules,
generates minimum heights, merges bitsets, reports geometry/surface/memory/hash,
and was actually executed at 400 km / 90 m. One local iteration measured 2.852 s
(27.71 M nominal cells-radar/s), 95,770 visible cells and deterministic hash
`fc7e022d262c6e91`; this is an observation, not a universal baseline or claimed
speedup.
The external SRTM path was verified end to end with `N45E002.hgt.gz`: a native
job downloaded and cached the 11,330,155-byte compressed tile, decoded it,
completed LOS, and wrote `.rcov`/`.rhgt`. After a full server restart, a second
job completed with the cache file size and modification timestamp unchanged,
proving persistent disk-cache reuse. WMTS `.png` routes use a full `{tile}`
segment and strictly parse the required suffix, as mandated by Axum 0.8.

The terrain crate now implements an explicit spherical azimuthal-equidistant
local projection centred on the radar. Radial distances are preserved and the
immutable SRTM mosaic can be resampled into a metric raster at 30, 90 or 180 m.
SRTM north-to-south row orientation is handled explicitly; missing tiles and
void values remain NoData rather than becoming zero elevation.

Not yet production-complete: reusable workspace pool and internal sector
parallelism for one or two radars, live WMTS map/profile widgets, deeper
integration/property tests, and repeated multi-scenario benchmark baselines. A
400 km / 30 m run was not attempted because it requires roughly 711 million
cells and several GiB for the standalone synthetic grid.
