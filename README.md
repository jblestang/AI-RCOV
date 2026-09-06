# Radial

Radial is a Rust workspace for terrain-based radar line-of-sight coverage. The
native server owns every scientific and SRTM operation; the WebAssembly client
only edits radar definitions, starts jobs and displays server-produced map tiles
and profiles.

> Status: foundational implementation. The LOS core, dynamic ray traversal,
> bitset fusion, HGT decoding, persistent envelope, PNG/LOD primitives,
> API contracts and server shell are implemented and tested. Network SRTM
> acquisition, complete asynchronous job execution, full WMTS routing and the
> production WASM map client remain integration work. See `docs/STATUS.md`.

## Scientific scope

The engine computes geometric intervisibility and minimum target height AGL. It
uses effective Earth curvature `h' = h - d²/(2kR)`, with `R = 6,371,000 m` and
default `k = 4/3`. It does not claim an RF link budget and does not model
diffraction, Fresnel clearance, free-space or atmospheric losses, clutter,
transmit power, receive power, or antenna patterns.

Internal loops use integer grid offsets in a documented local metric grid. A
production import must reproject geodetic SRTM samples to that local CRS before
calling `coverage-core`; longitude/cos(latitude) approximations are not used by
the core.

## Workspace

- `coverage-core`: dependency-free LOS, distance-ordered dynamic rays, profiles,
  bitsets and fusion.
- `terrain-srtm`: bounded SRTM downloading, two-level cache, strict
  SRTM-1/SRTM-3 HGT decoding and immutable shared mosaics.
- `coverage-storage`: versioned atomic `.rcov` / `.rhgt` envelopes and
  block-streamed height fusion.
- `radar-api`: shared JSON contracts.
- `radar-wmts`: semantic LOD reducers, PNG and ETag primitives.
- `radar-server`: bounded HTTP entry point.
- `radar-web`: presentation-only WASM boundary.
- `benchmark/standalone`: offline deterministic ray/LOS hot-loop benchmark.

## Development

```sh
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
rustc --edition=2021 -O benchmark/standalone/main.rs -o /tmp/radial-bench
RADAR_BENCH_CELL_M=90 /tmp/radial-bench
cargo run -p radar-server
```

The benchmark imports the production scientific source directly and therefore
cannot silently diverge into a second LOS implementation. On the development
machine, one LOS-v2 400 km / 90 m iteration completed in 1.780 s with hash
`c4138ec3d0639263`; compare performance only with repeated runs on the same CPU.

The server listens on `RADAR_BIND` (`0.0.0.0:8100` by default). Compile the web
crate with `RADAR_API_URL=https://radar.example`; a runtime override should be
provided by the hosting shell before production deployment.

SRTM downloads are performed only by `radar-server`. Set `RADAR_SRTM_CACHE` to
a persistent directory (default `data/srtm`); compressed `.hgt.gz` files survive
server restarts and are decoded locally on subsequent jobs. Client-provided
download URLs are never accepted.

Set the repository variable `RADAR_API_URL` to the public HTTPS server origin
before enabling GitHub Pages. The Pages workflow injects it into the runtime
meta tag and packages the Rust-generated WebAssembly. The container workflow
publishes `linux/amd64` and `linux/arm64` images to GHCR on `main` and version
tags.

## Deployment

`docker compose up --build` starts the native server. Persistent terrain and
result data is mounted at `/data`. GitHub Pages and the server are independent;
set an explicit HTTPS API origin and configure CORS to the exact Pages origin in
production. Immutable WMTS URLs include dataset/version/date, enabling one-year
cache headers without stale overwrites.

Dual licensed under MIT or Apache-2.0.

## Validation bout-en-bout

Avec le serveur accessible sur le port 8100, exécutez :

```sh
RADIAL_API_URL=http://127.0.0.1:8100 scripts/validate-e2e.sh
```

Le script attend health/readiness, crée un radar, déclenche le téléchargement
SRTM et le LOS, attend le job, crée une fusion, télécharge metadata et
GetCapabilities, puis sauvegarde et vérifie la pyramide complète de PNG `ground`, `agl-30m`,
`agl-50m`, `agl-100m`, hauteur personnalisée (75 m par défaut),
`min-detection-height` et `radar-count` pour tous les LOD, lignes et colonnes.
Les tuiles sont rangées sous `validation-output/tiles/<layer>/<z>/` et la
géométrie des matrices est écrite dans `tile-matrices.tsv`. Il vérifie également
ETag/304. `RADIAL_TILE_CONCURRENCY` borne les téléchargements parallèles (8 par
défaut).

Les requêtes strictement identiques sont idempotentes. Tant que le serveur reste
actif, un second `POST /jobs` renvoie le job existant. Les artefacts LOS sont
aussi indexés sur disque par la requête complète et validés par checksum après
un redémarrage. Une fusion de la même sélection à la même hauteur réutilise le
même identifiant et le même dataset WMTS persistant.

Les PNG sont des tuiles WMTS brutes : une petite grille de test occupe
normalement le coin supérieur gauche de la tuile 256 × 256. Le script génère
aussi `preview.html`, qui détecte la zone non nulle, la centre et l'agrandit pour
inspection. Les résultats sont placés dans `validation-output/`.

Pour un essai rapide :

```sh
RADIAL_API_URL=http://127.0.0.1:8100 \
RADIAL_RANGE_M=1000 \
RADIAL_RESOLUTION_M=180 \
RADIAL_TARGET_AGL_M=75 \
scripts/validate-e2e.sh
```
