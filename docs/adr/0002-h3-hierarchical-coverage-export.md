---
status: "accepted"
date: 2026-09-08
deciders: ["AI-RCOV Team"]
consulted: []
informed: ["Contributors", "AI Assistants"]
---

# 2. Hierarchical H3 Hexagonal Cell Export from Native Coverage Grids

## Context and Problem Statement

AI-RCOV computes radar line-of-sight (LOS) intervisibility and stores results as continuous binary raster envelopes (`.rhgt` and `.rcov` in `coverage-storage`), which are currently served as WMTS/TMS image tiles (e.g., PNG tiles at multiple levels of detail and altitude cuts).

Downstream consumer systems, geo-spatial query engines, and tactical visualization clients require discrete spatial indexing using Uber's [H3](https://h3geo.org/) global hexagonal grid. Specifically, for any radar position and range, clients need to query the **minimum visible altitude floor** across geodetic space as a stream of H3 cells (`.jsonl`).

How should AI-RCOV convert high-resolution radar raster coverage maps (up to 8891×8891 cells, ~79M pixels per radar) into discrete H3 cells with minimum altitude floors while maintaining high throughput, low memory footprint, and exact floor elevation semantics?

## Decision Drivers

* **Throughput and Latency**: Exporting a full 400 km radar coverage (79M pixels) must execute in sub-second to low-second timescales.
* **Algorithmic Efficiency**: Eliminate unnecessary raster processing for large masked or non-visible areas (mountain shadows, beyond-horizon regions, ocean).
* **Data Fidelity**: Retain the exact native minimum visible altitude floor ($u16$ meters) without lossy 8-bit quantization from intermediate images.
* **Decoupled Architecture**: Keep `coverage-core` dependency-free and `no_std`-friendly; encapsulate H3 dependencies in a dedicated crate.
* **Client Usability**: Precalculate and output hexagonal boundary polygons in WGS84 GeoJSON standard (`[lon, lat]`) so clients can render without bundling H3 libraries.

## Considered Options

* **Option 1: Ingest Pre-rendered WMTS/TMS PNG Tiles (Tile-Scraper)**: Read generated 256×256 PNG tiles across multiple altitude slices (0m, 100m, 200m...), uncompress PNGs, extract white pixels, and map to H3.
* **Option 2: Forward Push Pixel-by-Pixel Raster Traversal**: Iterate sequentially/in parallel through all 79 million pixels of the `.rhgt` raster, project each valid pixel to `(lat, lon)`, map to H3, and reduce via a global hash map.
* **Option 3: Reverse Pull with Top-Down Hierarchical Pruning (Chosen)**: Traverse candidate H3 cells from coarse resolution (Res 5) down to target resolution (Res 7). Prune subtrees where all underlying raster pixels are `NO_DATA`. In leaf cells, compute exact minimum floor and extract precalculated polygon boundary.

## Decision Outcome

Chosen option: **"Option 3: Reverse Pull with Top-Down Hierarchical Pruning"**, implemented with the pure-Rust **`h3o`** crate in a dedicated crate `coverage-h3`, because:
* It leverages spatial hierarchy: by checking coarse bounding boxes at Res 5/6, vast unobserved or shadow sectors are skipped completely without checking millions of individual pixels.
* For a 400 km radar at Resolution 7, there are only ~97,000 target cells vs 79,000,000 pixels, vastly reducing coordinate transform overhead.
* It operates directly on the native binary `.rhgt` raster, guaranteeing exact $u16$ metric precision and zero lossy intermediate steps.
* `h3o` provides pure Rust, zero-FFI, SIMD-accelerated H3 indexing compatible with multi-threaded `rayon` traversal.

### Consequences

* **Good**: Substantial speedup by skipping unvisited raster areas via top-down subtree pruning.
* **Good**: Decoupled crate architecture (`coverage-h3`) avoids polluting `coverage-core` with spatial indexing dependencies.
* **Good**: Precalculated GeoJSON-compatible polygon coordinates (`boundary`) enable instant rendering on web clients (Leaflet, MapLibre, Canvas) without client-side H3 dependencies.
* **Bad**: Requires geometric mapping between H3 cell boundaries and the local azimuthal equidistant projection (AEQD) bounding box.

## Validation

The decision will be validated by:
1. Unit tests in `crates/coverage-h3/tests/` verifying mathematical correctness against known coverage fixtures, determinism, NoData pruning, and polygon boundary integrity.
2. A standalone benchmark in `crates/coverage-h3` (or `benchmark/h3/`) measuring wall-clock latency, throughput (cells/s, Mpix/s), and memory consumption on a 400 km / 90 m radar dataset.

## Pros and Cons of the Options

### Option 1: Ingest Pre-rendered WMTS/TMS PNG Tiles (Tile-Scraper)

* Good: Independent from native raster formats; can run on external servers via HTTP.
* Bad: Huge CPU overhead due to PNG decompression.
* Bad: Lossy 8-bit altitude quantization.
* Bad: Requires scanning $N$ separate image layers for $N$ altitude buckets ($O(N \times \text{pixels})$).

### Option 2: Forward Push Pixel-by-Pixel Raster Traversal

* Good: Straightforward linear iteration over memory.
* Bad: Evaluates 79 million pixels individually, including tens of millions of hidden/NoData pixels.
* Bad: 79 million trigonometric coordinate transforms and H3 hash lookups.

### Option 3: Reverse Pull with Top-Down Hierarchical Pruning

* Good: Evaluates coarse cells first; whole unobserved regions are eliminated in $O(1)$ box checks.
* Good: Only ~100k target cells evaluated at Resolution 7 instead of 79M pixels.
* Good: Native $u16$ fidelity and single-pass execution.
* Bad: Slightly more complex logic for bounding-box to raster index projection.

## More Information

* [H3 Spatial Indexing System](https://h3geo.org/)
* [h3o Pure Rust Crate](https://crates.io/crates/h3o)
* [RFC 7946 GeoJSON Standard](https://datatracker.ietf.org/doc/html/rfc7946)
