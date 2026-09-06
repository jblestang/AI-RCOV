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

Metadata contains stable radar ID, hashes of radar configuration and terrain,
calculation timestamp, CRS, origin, source resolution, extent, dimensions,
range, effective-Earth factor and NoData value. Readers validate magic, version,
declared lengths and checksum before exposing content.

Square rings are strictly near-to-far. Circle/side intersections bound loop
intervals before callbacks, eliminating the square-envelope corner callbacks.
Four side ranges are disjoint and assign each corner once. Angular lookup stores
the first octant and reconstructs other octants by symmetry; interpolation has
an error bound and exact `atan2` is used near bin boundaries.

For lower WMTS levels, boolean and radar-count layers use `max` (`any` for
boolean); minimum-height uses `min` while ignoring NoData. Source resolution is
the maximum level. Edge samples outside odd-sized matrices are NoData.

