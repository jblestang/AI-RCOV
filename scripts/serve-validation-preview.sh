#!/usr/bin/env bash
set -euo pipefail

OUTPUT_DIR="${RADIAL_VALIDATION_DIR:-validation-output}"
PORT="${RADIAL_PREVIEW_PORT:-5173}"

command -v python3 >/dev/null || {
  echo "Commande requise absente: python3" >&2
  exit 2
}
[[ -f "$OUTPUT_DIR/preview.html" ]] || {
  echo "Aperçu absent: exécutez d'abord scripts/validate-e2e.sh" >&2
  exit 1
}

echo "Aperçu: http://localhost:${PORT}/preview.html"
exec python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$OUTPUT_DIR"
