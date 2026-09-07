#!/usr/bin/env bash
set -euo pipefail

OUTPUT_DIR="${RADIAL_VALIDATION_DIR:-validation-output}"
PORT="${RADIAL_PREVIEW_PORT:-8765}"

command -v python3 >/dev/null || {
  echo "Commande requise absente: python3" >&2
  exit 2
}
[[ -f "$OUTPUT_DIR/preview.html" ]] || {
  echo "Aperçu absent: exécutez d'abord scripts/validate-e2e.sh" >&2
  exit 1
}
if command -v lsof >/dev/null && lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "Le port $PORT est déjà occupé. Utilisez RADIAL_PREVIEW_PORT avec un port autorisé par RADAR_CORS_ORIGINS." >&2
  exit 1
fi

echo "Aperçu: http://localhost:${PORT}/preview.html"
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec python3 "$SCRIPT_DIR/serve-validation-preview.py" --port "$PORT" --directory "$OUTPUT_DIR"
