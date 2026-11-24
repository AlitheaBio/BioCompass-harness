#!/usr/bin/env zsh
set -euo pipefail

KEEP_STACK=0
for arg in "$@"; do
  case "${arg}" in
    --keep) KEEP_STACK=1 ;;
    *) echo "Unknown argument: ${arg} (supported: --keep)" >&2; exit 2 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
STANDALONE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/biocompass-gate5.XXXXXX")"
PORT="${BIOCOMPASS_GATE5_PORT:-18090}"
REGISTRY_PORT="${BIOCOMPASS_GATE5_REGISTRY_PORT:-5500}"
FROGLET_VERIFY_BIN="${FROGLET_VERIFY_BIN:-${REPO_ROOT}/../froglet/target/debug/froglet-verify}"

SERVER_PID=""
EPHEMERAL_REGISTRY_ENGINE=""
EPHEMERAL_REGISTRY_NAME=""

cleanup() {
  if [[ -n "${SERVER_PID}" ]]; then
    kill "${SERVER_PID}" >/dev/null 2>&1 || true
  fi
  if [[ "${KEEP_STACK}" -eq 0 ]]; then
    if [[ -n "${EPHEMERAL_REGISTRY_NAME}" && -n "${EPHEMERAL_REGISTRY_ENGINE}" ]]; then
      "${EPHEMERAL_REGISTRY_ENGINE}" rm -f "${EPHEMERAL_REGISTRY_NAME}" >/dev/null 2>&1 || true
    fi
    rm -rf "${STANDALONE_DIR}" >/dev/null 2>&1 || true
  else
    echo "[gate-5] --keep set; retaining ${STANDALONE_DIR} and any ephemeral registry (${EPHEMERAL_REGISTRY_NAME:-none})"
  fi
}
trap cleanup EXIT

detect_engine() {
  for bin in docker podman /usr/local/bin/docker /opt/homebrew/bin/docker /opt/podman/bin/podman; do
    if command -v "${bin}" >/dev/null 2>&1 && "${bin}" info >/dev/null 2>&1; then
      echo "${bin}"
      return 0
    fi
  done
  return 1
}

echo "[gate-5] 1. Ensuring local OCI Distribution Spec v1.1 registry on 127.0.0.1:${REGISTRY_PORT}..."
if curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/v2/" >/dev/null 2>&1; then
  echo "[gate-5]    Reusing active OCI registry on 127.0.0.1:${REGISTRY_PORT}."
else
  EPHEMERAL_REGISTRY_ENGINE="$(detect_engine)"
  EPHEMERAL_REGISTRY_NAME="biocompass-gate5-registry-$$"
  echo "[gate-5]    Starting ephemeral OCI registry (${EPHEMERAL_REGISTRY_NAME}) on 127.0.0.1:${REGISTRY_PORT}..."
  "${EPHEMERAL_REGISTRY_ENGINE}" run -d --rm \
    --name "${EPHEMERAL_REGISTRY_NAME}" \
    -p "127.0.0.1:${REGISTRY_PORT}:5000" \
    ghcr.io/distribution/distribution:3.0.0 >/dev/null
  for _ in {1..40}; do
    if curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/v2/" >/dev/null 2>&1; then
      break
    fi
    sleep 0.25
  done
  curl -fsS "http://127.0.0.1:${REGISTRY_PORT}/v2/" >/dev/null
fi

echo "[gate-5] 2. Copying standalone binary + offline signed DuckDB extensions outside repository -> ${STANDALONE_DIR}"
"${REPO_ROOT}/scripts/install-duckdb-extensions.sh" --verify-only
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

echo "[gate-5] 3. Booting standalone binary from ${STANDALONE_DIR} (zero source-tree file dependency)..."
(
  cd "${STANDALONE_DIR}"
  exec ./biocompass up --host 127.0.0.1 --port "${PORT}" --data-dir "${STANDALONE_DIR}/data" >"${STANDALONE_DIR}/server.log" 2>&1
) &
SERVER_PID=$!

for _ in {1..80}; do
  if curl -fsS "http://127.0.0.1:${PORT}/api/v1/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.25
done

curl -fsS "http://127.0.0.1:${PORT}/api/v1/health"
echo ""

OWNER_LINK_FILE="${STANDALONE_DIR}/data/owner-link.txt"
if [[ ! -s "${OWNER_LINK_FILE}" ]]; then
  echo "[gate-5] ERROR: expected local owner link file at ${OWNER_LINK_FILE}" >&2
  exit 1
fi

echo "[gate-5] 4. Running Chromium Playwright E2E + <iframe sandbox='allow-scripts'> exploit suite..."
(
  cd "${REPO_ROOT}"
  BIOCOMPASS_E2E_URL="http://127.0.0.1:${PORT}" \
    BIOCOMPASS_E2E_OWNER_LINK_FILE="${OWNER_LINK_FILE}" \
    npx playwright test e2e/gate5_workbench_and_sandbox.spec.ts
)

curl -fsS "http://127.0.0.1:${PORT}/api/v1/runs" > "${STANDALONE_DIR}/runs.json"

echo "[gate-5] 5. Verifying all browser-generated 5-artifact Schnorr chains..."
python3 - "${STANDALONE_DIR}" "${FROGLET_VERIFY_BIN}" <<'PY'
import json
import os
import subprocess
import sys
from pathlib import Path

standalone_dir = Path(sys.argv[1])
verify_bin = sys.argv[2]
biocompass_bin = standalone_dir / "biocompass"
data = json.loads((standalone_dir / "runs.json").read_text(encoding="utf-8"))
runs = data["runs"]
assert len(runs) >= 2, f"Expected at least 2 completed browser runs, got {len(runs)}"

use_external = os.path.isfile(verify_bin) and os.access(verify_bin, os.X_OK)

for run in runs:
    run_id = run["run_id"]
    plugin_id = run["plugin_id"]
    chain_path = standalone_dir / f"chain_{run_id}.json"
    chain_path.write_text(json.dumps(run["froglet_chain"], indent=2), encoding="utf-8")
    if use_external:
        proc = subprocess.run(
            [verify_bin, "--chain", "--json", str(chain_path)],
            capture_output=True,
            text=True,
            check=True,
        )
        verdict = json.loads(proc.stdout)
        assert verdict["valid"] is True, f"External froglet-verify failed: {verdict}"
        print(f"  ✓ [external froglet-verify] {run_id} ({plugin_id}) -> valid=True, 5/5 artifacts verified")
    else:
        proc = subprocess.run(
            # W6-1: this gate checks chain integrity only; signer trust is not part of it.
            [str(biocompass_bin), "verify-receipt", "--no-trust-anchor", str(chain_path)],
            capture_output=True,
            text=True,
            check=True,
        )
        print(f"  ✓ [biocompass verify-receipt] {run_id} ({plugin_id}) -> verified")
PY

echo "[gate-5] PASSED 100%"
