# Acceptance status

This first repository revision is deliberately honest about incomplete work.

Implemented: network-free LOS core, `k` correction, minimum AGL, unique clipped
rings, angular lookup/exact fallback, bitsets and saturating fusion, profiles,
strict HGT decoding, immutable `Arc<HgtTile>` mosaic, binary envelopes with
atomic writes/checksum, API DTOs, health/readiness and radar CRUD shell,
semantic LOD/PNG/ETag primitives, offline benchmark, container and CI skeleton.

Not yet production-complete: bounded downloader and two-level SRTM cache,
reprojection/mosaic resampling, Rayon work scheduling and workspace pool,
streaming persistent fusion, full job lifecycle/cancellation, complete WMTS REST
routes/capabilities/cache, browser map and profiles, runtime CORS allow-list,
integration/property tests, and production benchmark baselines. A 400 km / 30 m
run was not attempted because it requires roughly 711 million cells and must be
guarded by the future memory scheduler.

