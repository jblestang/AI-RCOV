# Architecture and binary formats

The dependency direction is `radar-web -> radar-api` and
`radar-server -> {radar-api, terrain-srtm, coverage-core, coverage-storage,
radar-wmts}`. Scientific code has no HTTP, filesystem or browser dependency.

Each `.rcov` and `.rhgt` file contains: four-byte magic, little-endian `u16`
format version, `u32` JSON-header length, `u64` payload length, UTF-8 JSON
metadata, payload, and a 32-byte BLAKE3 checksum over magic/version/header/
payload. `.rcov` payloads contain `u64` bitset words; `.rhgt` payloads contain
little-endian `u16` values. `65535` is reserved for NoData/out-of-range and real
heights saturate at `65534`. Writers use a sibling temporary file, flush,
optional fsync, and atomic rename.

Metadata contains stable radar ID, an explicit LOS algorithm version, hashes of radar configuration and terrain,
calculation timestamp, CRS, origin, source resolution, extent, dimensions,
range, effective-Earth factor and NoData value. Readers validate magic, version,
declared lengths and checksum before exposing content.

Angular rays traverse intersected grid cells in increasing distance using
cell-boundary DDA. Their angular spacing is derived from one grid cell at the
maximum range, so nearby terrain cells affect multiple rays through their full
footprint. Each ray owns an independent horizon and duplicate cell observations
retain the conservative maximum required AGL.

Terrain preparation uses a radar-centred spherical azimuthal-equidistant
projection (`+proj=aeqd`, `R=6371000`). Unlike a longitude/cos(latitude)
approximation, this preserves distance from the radar centre over the complete
400 km disk. SRTM samples are converted once into an immutable metric raster;
all hot LOS loops subsequently use integer indices and metre resolution.

For lower WMTS levels, boolean and radar-count layers use `max` (`any` for
boolean); minimum-height uses `min` while ignoring NoData. Source resolution is
the maximum level. Edge samples outside odd-sized matrices are NoData.
