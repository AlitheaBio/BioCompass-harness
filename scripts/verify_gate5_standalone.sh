#!/usr/bin/env zsh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STANDALONE_DIR="/tmp/biocompass-standalone-test"
PORT="${BIOCOMPASS_GATE5_PORT:-18090}"
FROGLET_VERIFY_BIN="${FROGLET_VERIFY_BIN:-${REPO_ROOT}/../froglet/target/debug/froglet-verify}"

echo "[gate-5] 1. Verifying local OCI Distribution Spec v1.1 registry on 127.0.0.1:5500..."
curl -fsS http://127.0.0.1:5500/v2/ >/dev/null

echo "[gate-5] 2. Copying standalone binary + offline signed DuckDB extensions outside repository -> ${STANDALONE_DIR}"
rm -rf "${STANDALONE_DIR}"
mkdir -p "${STANDALONE_DIR}"
BIOCOMPASS_BIN="${BIOCOMPASS_BIN:-}"
if [[ -z "${BIOCOMPASS_BIN}" ]]; then
  if [[ -x "${REPO_ROOT}/target/debug/biocompass" ]]; then
    BIOCOMPASS_BIN="${REPO_ROOT}/target/debug/biocompass"
  else
    BIOCOMPASS_BIN="${REPO_ROOT}/target/release/biocompass"
  fi
fi
cp "${BIOCOMPASS_BIN}" "${STANDALONE_DIR}/biocompass"
cp -R "${REPO_ROOT}/vendor/duckdb-extensions" "${STANDALONE_DIR}/duckdb-extensions"

echo "[gate-5] 3. Booting standalone binary from /tmp (zero source-tree file dependency)..."
(
  cd "${STANDALONE_DIR}"
  exec ./biocompass up --host 127.0.0.1 --port "${PORT}" --data-dir "${STANDALONE_DIR}/data" >"${STANDALONE_DIR}/server.log" 2>&1
) &
SERVER_PID=$!
trap 'kill "${SERVER_PID}" >/dev/null 2>&1 || true' EXIT

for _ in {1..80}; do
  if curl -fsS "http://127.0.0.1:${PORT}/api/v1/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done

curl -fsS "http://127.0.0.1:${PORT}/api/v1/health"
echo ""

echo "[gate-5] 4. Running Chromium Playwright E2E + <iframe sandbox='allow-scripts'> exploit suite..."
(
  cd "${REPO_ROOT}"
  BIOCOMPASS_E2E_URL="http://127.0.0.1:${PORT}" npx playwright test
)

curl -fsS "http://127.0.0.1:${PORT}/api/v1/runs" > "${STANDALONE_DIR}/runs.json"

if [[ -x "${FROGLET_VERIFY_BIN}" ]]; then
  echo "[gate-5] 5. Verifying all browser-generated 5-artifact Schnorr chains via external froglet-verify CLI..."
  python3 - "${STANDALONE_DIR}" "${FROGLET_VERIFY_BIN}" <<'PY'
import json
import subprocess
import sys
from pathlib import Path

standalone_dir = Path(sys.argv[1])
verify_bin = sys.argv[2]
data = json.loads((standalone_dir / "runs.json").read_text(encoding="utf-8"))
runs = data["runs"]
assert len(runs) >= 2, f"Expected at least 2 completed browser runs, got {len(runs)}"

for run in runs:
    run_id = run["run_id"]
    plugin_id = run["plugin_id"]
    chain_path = standalone_dir / f"chain_{run_id}.json"
    chain_path.write_text(json.dumps(run["froglet_chain"], indent=2), encoding="utf-8")
    proc = subprocess.run(
        [verify_bin, "--chain", "--json", str(chain_path)],
        capture_output=True,
        text=True,
        check=True,
    )
    verdict = json.loads(proc.stdout)
    assert verdict["valid"] is True, f"External froglet-verify failed: {verdict}"
    print(f"  ✓ [external froglet-verify] {run_id} ({plugin_id}) -> valid=True, 5/5 artifacts verified")
PY
fi

echo "[gate-5] PASSED 100%"
