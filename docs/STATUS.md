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
Until terrain reprojection is connected, workers fail explicitly instead of
publishing a synthetic or scientifically misleading completed coverage.

Not yet production-complete: reprojection/mosaic resampling, reusable workspace
pool and internal sector parallelism for one or two radars,
scientific execution inside the job worker, complete WMTS REST
routes/capabilities/cache, browser map and profiles, runtime CORS allow-list,
integration/property tests, and production benchmark baselines. A 400 km / 30 m
run was not attempted because it requires roughly 711 million cells and must be
guarded by the future memory scheduler.
