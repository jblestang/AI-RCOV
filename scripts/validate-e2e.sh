#!/usr/bin/env bash
set -euo pipefail

API_URL="${RADIAL_API_URL:-http://127.0.0.1:8100}"
OUTPUT_DIR="${RADIAL_VALIDATION_DIR:-validation-output}"
TARGET_AGL_M="${RADIAL_TARGET_AGL_M:-75}"
LATITUDE="${RADIAL_LATITUDE:-45.2}"
LONGITUDE="${RADIAL_LONGITUDE:-2.2}"
RANGE_M="${RADIAL_RANGE_M:-5000}"
RESOLUTION_M="${RADIAL_RESOLUTION_M:-180}"
TIMEOUT_SECONDS="${RADIAL_TIMEOUT_SECONDS:-300}"
RADAR_ID="${RADIAL_RADAR_ID:-11111111-1111-4111-8111-111111111111}"

for command in curl jq od; do command -v "$command" >/dev/null || { echo "Commande requise absente: $command" >&2; exit 2; }; done
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

echo "[6/7] Téléchargement des couches PNG"
layers=(ground agl-30m agl-50m agl-100m "agl-${TARGET_AGL_M}m" min-detection-height radar-count)
for layer in "${layers[@]}"; do
  png="$OUTPUT_DIR/${layer}.png"
  headers="$OUTPUT_DIR/${layer}.headers"
  curl -fsS -D "$headers" "$base/$layer/0/0/0.png" -o "$png"
  signature=$(od -An -tx1 -N8 "$png" | tr -d ' \n')
  [[ "$signature" == 89504e470d0a1a0a ]] || { echo "Signature PNG invalide: $layer" >&2; exit 1; }
done

echo "[7/7] Validation ETag / 304"
etag=$(awk 'tolower($1)=="etag:"{gsub("\r","");print $2}' "$OUTPUT_DIR/radar-count.headers")
[[ -n "$etag" ]] || { echo "ETag absent" >&2; exit 1; }
code=$(curl -sS -o /dev/null -w '%{http_code}' -H "If-None-Match: $etag" "$base/radar-count/0/0/0.png")
[[ "$code" == 304 ]] || { echo "304 attendu, reçu $code" >&2; exit 1; }

echo "VALIDATION OK"
echo "Job: $job_id"
echo "Dataset: $dataset_id"
echo "PNG: $OUTPUT_DIR"
