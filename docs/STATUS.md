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
The native job worker is connected end to end for mono-radar artifacts: it
enumerates and downloads all SRTM tiles, builds an immutable mosaic, projects a
metric grid, executes LOS in a blocking native worker, and atomically persists
both `.rcov` and `.rhgt`. A job reaches `completed` only after both validated
artifacts exist for every requested radar; cancellation is checked between
expensive stages.

The terrain crate now implements an explicit spherical azimuthal-equidistant
local projection centred on the radar. Radial distances are preserved and the
immutable SRTM mosaic can be resampled into a metric raster at 30, 90 or 180 m.
SRTM north-to-south row orientation is handled explicitly; missing tiles and
void values remain NoData rather than becoming zero elevation.

Not yet production-complete: common-grid multi-radar job planning, content-based
terrain hashing, reusable workspace pool and internal sector parallelism for one
or two radars, complete WMTS REST
routes/capabilities/cache, browser map and profiles, runtime CORS allow-list,
integration/property tests, and production benchmark baselines. A 400 km / 30 m
run was not attempted because it requires roughly 711 million cells and must be
guarded by the future memory scheduler.
