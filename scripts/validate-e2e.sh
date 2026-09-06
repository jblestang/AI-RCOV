#!/usr/bin/env bash
set -euo pipefail

API_URL="${RADIAL_API_URL:-http://127.0.0.1:8100}"
OUTPUT_DIR="${RADIAL_VALIDATION_DIR:-validation-output}"
TARGET_AGL_M="${RADIAL_TARGET_AGL_M:-75}"
LATITUDE="${RADIAL_LATITUDE:-45.2}"
LONGITUDE="${RADIAL_LONGITUDE:-2.2}"
RANGE_M="${RADIAL_RANGE_M:-20000}"
RESOLUTION_M="${RADIAL_RESOLUTION_M:-180}"
TIMEOUT_SECONDS="${RADIAL_TIMEOUT_SECONDS:-300}"
TILE_CONCURRENCY="${RADIAL_TILE_CONCURRENCY:-8}"
RADAR_ID="${RADIAL_RADAR_ID:-11111111-1111-4111-8111-111111111111}"

for command in curl jq od; do command -v "$command" >/dev/null || { echo "Commande requise absente: $command" >&2; exit 2; }; done
[[ "$TILE_CONCURRENCY" =~ ^[1-9][0-9]*$ ]] || { echo "RADIAL_TILE_CONCURRENCY doit être un entier positif" >&2; exit 2; }
mkdir -p "$OUTPUT_DIR"

echo "[1/7] Attente du serveur $API_URL"
for _ in $(seq 1 30); do curl -fsS "$API_URL/health" >/dev/null && break; sleep 1; done
curl -fsS "$API_URL/ready" >/dev/null

radar_json=$(jq -n --arg id "$RADAR_ID" --argjson lat "$LATITUDE" --argjson lon "$LONGITUDE" --argjson range "$RANGE_M" '{id:$id,name:"Validation SRTM",latitude:$lat,longitude:$lon,antenna_agl_m:20,range_m:$range,active:true}')
echo "[2/7] Création/mise à jour du radar"
curl -fsS -H 'content-type: application/json' -d "$radar_json" "$API_URL/api/v1/radars" >"$OUTPUT_DIR/radar.json"

job_json=$(jq -n --argjson radar "$radar_json" --argjson resolution "$RESOLUTION_M" '{radars:[$radar],resolution_m:$resolution,effective_earth_k:1.3333333333333333,target_heights_agl_m:[30,50,100]}')
echo "[3/7] Job SRTM + LOS"
curl -fsS -H 'content-type: application/json' -d "$job_json" "$API_URL/api/v1/jobs" >"$OUTPUT_DIR/job-created.json"
job_id=$(jq -er '.id' "$OUTPUT_DIR/job-created.json")
deadline=$((SECONDS + TIMEOUT_SECONDS))
while (( SECONDS < deadline )); do
  curl -fsS "$API_URL/api/v1/jobs/$job_id" >"$OUTPUT_DIR/job-final.json"
  state=$(jq -er '.state' "$OUTPUT_DIR/job-final.json")
  case "$state" in completed) break;; failed|cancelled) jq . "$OUTPUT_DIR/job-final.json"; exit 1;; esac
  sleep 2
done
[[ "${state:-}" == completed ]] || { echo "Timeout du job $job_id" >&2; exit 1; }

echo "[4/7] Fusion à ${TARGET_AGL_M} m AGL"
fusion_json=$(jq -n --arg id "$RADAR_ID" --argjson height "$TARGET_AGL_M" '{radar_ids:[$id],target_agl_m:$height}')
curl -fsS -H 'content-type: application/json' -d "$fusion_json" "$API_URL/api/v1/fusions" >"$OUTPUT_DIR/fusion.json"
dataset_id=$(jq -er '.fusion_id' "$OUTPUT_DIR/fusion.json")
metadata_path=$(jq -er '.dataset_url' "$OUTPUT_DIR/fusion.json")
date=$(awk -F/ '{print $(NF-1)}' <<<"$metadata_path")
base="$API_URL/wmts/$dataset_id/1/$date"

echo "[5/7] Métadonnées et GetCapabilities"
curl -fsS "$base/metadata.json" -o "$OUTPUT_DIR/metadata.json"
curl -fsS "$base/WMTSCapabilities.xml" -o "$OUTPUT_DIR/WMTSCapabilities.xml"
grep -q '<Capabilities' "$OUTPUT_DIR/WMTSCapabilities.xml"
width=$(jq -er '.metadata.width' "$OUTPUT_DIR/metadata.json")
height=$(jq -er '.metadata.height' "$OUTPUT_DIR/metadata.json")

echo "[6/7] Téléchargement de la pyramide PNG complète"
layers=()
for candidate in ground agl-30m agl-50m agl-100m "agl-${TARGET_AGL_M}m" min-detection-height radar-count; do
  [[ " ${layers[*]:-} " == *" $candidate "* ]] || layers+=("$candidate")
done
max_size=$width
(( height > max_size )) && max_size=$height
levels=1
while (( max_size > 256 )); do max_size=$(( (max_size + 1) / 2 )); levels=$((levels + 1)); done
: >"$OUTPUT_DIR/tile-matrices.tsv"
tile_count=0
pids=()
download_tile() {
  local layer=$1 z=$2 row=$3 col=$4 destination=$5
  curl -fsS "$base/$layer/$z/$row/$col.png" -o "$destination"
  local signature
  signature=$(od -An -tx1 -N8 "$destination" | tr -d ' \n')
  [[ "$signature" == 89504e470d0a1a0a ]]
}
for ((z=0; z<levels; z++)); do
  factor=$((1 << (levels - 1 - z)))
  span=$((256 * factor))
  matrix_width=$(( (width + span - 1) / span ))
  matrix_height=$(( (height + span - 1) / span ))
  printf '%s\t%s\t%s\t%s\n' "$z" "$factor" "$matrix_width" "$matrix_height" >>"$OUTPUT_DIR/tile-matrices.tsv"
  echo "      LOD $z: ${matrix_width}x${matrix_height} tuiles, facteur $factor"
  for layer in "${layers[@]}"; do
    directory="$OUTPUT_DIR/tiles/$layer/$z"
    mkdir -p "$directory"
    for ((row=0; row<matrix_height; row++)); do
      for ((col=0; col<matrix_width; col++)); do
        download_tile "$layer" "$z" "$row" "$col" "$directory/${row}-${col}.png" &
        pids+=("$!")
        tile_count=$((tile_count + 1))
        if (( ${#pids[@]} >= TILE_CONCURRENCY )); then
          for pid in "${pids[@]}"; do wait "$pid"; done
          pids=()
        fi
      done
    done
  done
done
for pid in "${pids[@]}"; do wait "$pid"; done
for layer in "${layers[@]}"; do
  curl -fsS -D "$OUTPUT_DIR/${layer}.headers" "$base/$layer/0/0/0.png" -o "$OUTPUT_DIR/${layer}.png"
done

cat >"$OUTPUT_DIR/preview.html" <<HTML
<!doctype html><meta charset="utf-8"><title>Radial validation</title>
<style>body{font:14px system-ui;background:#071116;color:#e9f1f4;margin:24px}main{display:grid;grid-template-columns:repeat(auto-fit,minmax(290px,1fr));gap:18px}.card{background:#10232b;border:1px solid #29444e;border-radius:12px;padding:14px}canvas{display:block;width:256px;height:256px;background:#08151a;border:1px solid #34505b;image-rendering:pixelated;margin:auto}</style>
<h1>Radial · pyramide ${width} × ${height}</h1><p>${levels} LOD, ${tile_count} tuiles téléchargées. Chaque carte montre la tuile row 0 / col 0 du LOD.</p><main id="cards"></main>
<script>const layers='${layers[*]}'.split(' ');const levels=${levels};for(const name of layers)for(let z=0;z<levels;z++){const card=document.createElement('section');card.className='card';const title=document.createElement('h2');title.textContent=name+' · LOD '+z;const image=new Image();image.src='tiles/'+name+'/'+z+'/0-0.png';card.append(title,image);cards.append(card);}</script>
HTML

echo "[7/7] Validation ETag / 304"
etag=$(awk 'tolower($1)=="etag:"{gsub("\r","");print $2}' "$OUTPUT_DIR/radar-count.headers")
[[ -n "$etag" ]] || { echo "ETag absent" >&2; exit 1; }
code=$(curl -sS -o /dev/null -w '%{http_code}' -H "If-None-Match: $etag" "$base/radar-count/0/0/0.png")
[[ "$code" == 304 ]] || { echo "304 attendu, reçu $code" >&2; exit 1; }

echo "VALIDATION OK"
echo "Job: $job_id"
echo "Dataset: $dataset_id"
echo "Pyramide PNG: $tile_count tuiles, $levels LOD dans $OUTPUT_DIR/tiles"
echo "Matrices: $OUTPUT_DIR/tile-matrices.tsv"
echo "Aperçu par LOD: $OUTPUT_DIR/preview.html"
