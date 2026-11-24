#!/usr/bin/env bash
# U-3: measure click-to-usable module UI time against REAL processes:
#   - two compass-hub processes (hub A = the connected workspace hub, hub B = an approved
#     federated provider peer of hub A), on Postgres + Garage S3,
#   - the real `biocompass` harness binary with an empty UI cache,
#   - a TCP proxy adding latency between the harness and both hubs (and hub A -> hub B).
# The timer starts at the Open click and stops when the module UI has completed the plugin
# SDK `biocompass.getContext` handshake.
#
# One command:  e2e/u3_real_hubs/run.sh
# Env: U3_ONE_WAY_DELAY_MS (default 30 => 60 ms RTT), U3_SKIP_BUILD=1, U3_KEEP=1 (keep processes).
# Requires: docker with container `c1fix-pg` (Postgres 16 on 127.0.0.1:15432) or PG_* overrides,
# the `dxflrs/garage:v1.0.1` image, node, and Playwright's chromium.
set -euo pipefail

H="$(cd "$(dirname "$0")/../.." && pwd)"
C="${BIOCOMPASS_CLOUD_DIR:-$H/../biocompass-cloud}"
HERE="$H/e2e/u3_real_hubs"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/u3-real-hubs.XXXXXX")"
DELAY="${U3_ONE_WAY_DELAY_MS:-30}"

PG_CONTAINER="${PG_CONTAINER:-c1fix-pg}"
PG_URL_BASE="${PG_URL_BASE:-postgres://biocompass:biocompass_dev_password@127.0.0.1:15432}"
GARAGE_NAME="u3-garage-$$"
S3_PORT="${U3_S3_PORT:-19310}"
REG_PORT=28150
HUB_A_PORT=28101
HUB_B_PORT=28102
PROXY_A_PORT=28111
PROXY_B_PORT=28112
HARNESS_PORT=28190
S3_KEY=GK0123456789abcdef01234567  # gitleaks:allow (placeholder for a throwaway local Garage container)
S3_SECRET=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  # gitleaks:allow (placeholder)
KEY_A="bc_live_$(openssl rand -hex 24)"
KEY_B="bc_live_$(openssl rand -hex 24)"
PIDS=()

cleanup() {
  if [[ "${U3_KEEP:-0}" == "1" ]]; then
    echo "U3_KEEP=1: leaving processes running (work dir $WORK)"
    return
  fi
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  docker rm -f "$GARAGE_NAME" >/dev/null 2>&1 || true
  docker exec "$PG_CONTAINER" dropdb -U biocompass --if-exists biocompass_u_a >/dev/null 2>&1 || true
  docker exec "$PG_CONTAINER" dropdb -U biocompass --if-exists biocompass_u_b >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

wait_http() {
  local url="$1" name="$2"
  for _ in $(seq 1 120); do
    if curl -fsS "$url" >/dev/null 2>&1; then return 0; fi
    sleep 0.5
  done
  echo "timed out waiting for $name ($url)" >&2
  return 1
}

if [[ "${U3_SKIP_BUILD:-0}" != "1" ]]; then
  echo "== building compass-hub and biocompass (with web/dist)"
  (cd "$C" && CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo build -p compass-hub)
  (cd "$H" && npm run build >/dev/null && cargo build --bin biocompass)
fi
HUB_BIN="$C/target/debug/compass-hub"
BC_BIN="$H/target/debug/biocompass"

echo "== Postgres databases"
for db in biocompass_u_a biocompass_u_b; do
  docker exec "$PG_CONTAINER" dropdb -U biocompass --if-exists "$db"
  docker exec "$PG_CONTAINER" createdb -U biocompass "$db"
done

echo "== Garage S3 ($GARAGE_NAME on 127.0.0.1:$S3_PORT)"
docker run -d --name "$GARAGE_NAME" --hostname garage -p "127.0.0.1:$S3_PORT:3900" \
  -v "$C/docker/garage.toml:/etc/garage.toml:ro" dxflrs/garage:v1.0.1 \
  /garage -c /etc/garage.toml server >/dev/null
gexec() { docker exec "$GARAGE_NAME" /garage -c /etc/garage.toml "$@"; }
for _ in $(seq 1 60); do gexec status >/dev/null 2>&1 && break; sleep 0.5; done
NODE_ID="$(gexec node id -q | cut -d@ -f1)"
gexec layout assign -z eu-central-1 -c 1G "$NODE_ID" >/dev/null
gexec layout apply --version 1 >/dev/null
gexec key import --yes -n u3 "$S3_KEY" "$S3_SECRET" >/dev/null
for b in u3-hub-a u3-hub-b; do
  gexec bucket create "$b" >/dev/null
  gexec bucket allow --read --write --owner "$b" --key "$S3_KEY" >/dev/null
done

echo "== fake OCI registry, test modules"
node "$HERE/fake_registry.mjs" "$REG_PORT" "$WORK/digests.json" alithea/u3-cloud alithea/u3-network &
PIDS+=($!)
wait_http "http://127.0.0.1:$REG_PORT/v2/" "registry"
DIG_CLOUD="$(node -e "console.log(require('$WORK/digests.json')['alithea/u3-cloud'])")"
DIG_NET="$(node -e "console.log(require('$WORK/digests.json')['alithea/u3-network'])")"
node "$HERE/make_module.mjs" "$WORK/mod-cloud" alithea/u3-cloud "U3 Cloud Module" \
  "127.0.0.1:$REG_PORT/alithea/u3-cloud@$DIG_CLOUD"
node "$HERE/make_module.mjs" "$WORK/mod-network" alithea/u3-network "U3 Network Module" \
  "127.0.0.1:$REG_PORT/alithea/u3-network@$DIG_NET"

start_hub() {
  local name="$1" port="$2" db="$3" bucket="$4" key="$5"; shift 5
  mkdir -p "$WORK/$name"
  env DATABASE_URL="$PG_URL_BASE/$db" \
    BIOCOMPASS_S3_ENDPOINT="http://127.0.0.1:$S3_PORT" \
    BIOCOMPASS_S3_PUBLIC_ENDPOINT="http://127.0.0.1:$S3_PORT" \
    BIOCOMPASS_S3_REGION=eu-central-1 BIOCOMPASS_S3_BUCKET="$bucket" \
    BIOCOMPASS_S3_ACCESS_KEY="$S3_KEY" BIOCOMPASS_S3_SECRET_KEY="$S3_SECRET" \
    COMPASS_HUB_BOOTSTRAP_API_KEY="$key" COMPASS_HUB_STATE_DIR="$WORK/$name/state" \
    COMPASS_HUB_OCI_INSECURE_HTTP=true COMPASS_FEDERATION_ALLOW_INSECURE_DEV=1 \
    COMPASS_DEFAULT_MARKETPLACE_HUB_URL="http://127.0.0.1:1" \
    "$@" "$HUB_BIN" --host 127.0.0.1 --port "$port" >"$WORK/$name.log" 2>&1 &
  PIDS+=($!)
  wait_http "http://127.0.0.1:$port/api/v1/health" "$name" || { tail -40 "$WORK/$name.log"; exit 1; }
}

echo "== hub B (federated provider) on :$HUB_B_PORT"
start_hub hub-b "$HUB_B_PORT" biocompass_u_b u3-hub-b "$KEY_B" \
  COMPASS_FEDERATION_ID=hub-b COMPASS_FEDERATION_ORIGIN="http://127.0.0.1:$PROXY_B_PORT"
HUB_B_KEY="$(curl -fsS "http://127.0.0.1:$HUB_B_PORT/v1/hub-identity" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).hub_key))')"

echo "== latency proxies (+${DELAY} ms each way)"
node "$HERE/latency_proxy.mjs" "$PROXY_A_PORT" 127.0.0.1 "$HUB_A_PORT" "$DELAY" & PIDS+=($!)
node "$HERE/latency_proxy.mjs" "$PROXY_B_PORT" 127.0.0.1 "$HUB_B_PORT" "$DELAY" & PIDS+=($!)

echo "== publish the network module on hub B with a global run offer"
# Register pending review and upload the UI, then approve (public approval requires the UI).
for vis in public_pending_review public_approved; do
  "$BC_BIN" publish-plugin --dir "$WORK/mod-network" --hub-url "http://127.0.0.1:$HUB_B_PORT" \
    --api-key "$KEY_B" --visibility "$vis" >"$WORK/publish-b-$vis.json"
done
curl -fsS -X POST "http://127.0.0.1:$HUB_B_PORT/api/v1/module-offers" -H "X-API-Key: $KEY_B" \
  -H 'Content-Type: application/json' \
  -d '{"plugin_id":"alithea/u3-network","version":"1.0.0","action":"run","title":"U3 Network Module","scope":"global"}' \
  >"$WORK/offer-b.json"

echo "== hub A (connected workspace hub) on :$HUB_A_PORT with hub B as approved provider peer"
PEERS="[{\"id\":\"hub-b\",\"label\":\"Hub B\",\"role\":\"provider\",\"origin\":\"http://127.0.0.1:$PROXY_B_PORT\",\"backchannel\":\"http://127.0.0.1:$PROXY_B_PORT\",\"froglet_pubkey\":\"$HUB_B_KEY\"}]"
start_hub hub-a "$HUB_A_PORT" biocompass_u_a u3-hub-a "$KEY_A" \
  COMPASS_FEDERATION_ID=hub-a COMPASS_FEDERATION_ORIGIN="http://127.0.0.1:$PROXY_A_PORT" \
  COMPASS_FEDERATION_PEERS="$PEERS"
"$BC_BIN" publish-plugin --dir "$WORK/mod-cloud" --hub-url "http://127.0.0.1:$HUB_A_PORT" \
  --api-key "$KEY_A" --visibility org_private >"$WORK/publish-a.json"
# Trigger hub A's first sync of hub B's feed (federated offers).
curl -fsS "http://127.0.0.1:$HUB_A_PORT/api/v1/auth/federation/offers" -H "X-API-Key: $KEY_A" >"$WORK/fed-offers.json"
curl -fsS "http://127.0.0.1:$HUB_A_PORT/api/v1/network-modules" -H "Authorization: Bearer $KEY_A" >"$WORK/network-modules.json"
node -e "const m=require('$WORK/network-modules.json').modules; if(!m.some(x=>x.plugin_id==='alithea/u3-network')){console.error('hub A lists no federated module from hub B',JSON.stringify(m));process.exit(1)}"

echo "== harness (empty UI cache, prefetch disabled for the cold case) on :$HARNESS_PORT"
mkdir -p "$WORK/harness"
env COMPASS_AIRGAPPED=0 CI= BIOCOMPASS_UI_PREFETCH=0 \
  COMPASS_DEFAULT_MARKETPLACE_HUB_URL="http://127.0.0.1:$PROXY_A_PORT" \
  "$BC_BIN" up --host 127.0.0.1 --port "$HARNESS_PORT" --data-dir "$WORK/harness" \
  >"$WORK/harness.log" 2>&1 </dev/null &
PIDS+=($!)
wait_http "http://127.0.0.1:$HARNESS_PORT/api/v1/health" harness || { tail -40 "$WORK/harness.log"; exit 1; }
for _ in $(seq 1 20); do [[ -s "$WORK/harness/owner-link.txt" ]] && break; sleep 0.25; done

echo "== Playwright measurement"
(cd "$H" && U3_REAL_HUBS=1 BIOCOMPASS_E2E_URL="http://127.0.0.1:$HARNESS_PORT" \
  U3_OWNER_LINK_FILE="$WORK/harness/owner-link.txt" U3_HUB_A_KEY="$KEY_A" \
  U3_HUB_A_URL="http://127.0.0.1:$PROXY_A_PORT" U3_ONE_WAY_DELAY_MS="$DELAY" \
  U3_RESULTS_FILE="${U3_RESULTS_FILE:-$H/e2e/u3_real_hubs/last_results.json}" \
  npx playwright test e2e/u3_real_hubs.spec.ts --reporter=list)
echo "results: ${U3_RESULTS_FILE:-$H/e2e/u3_real_hubs/last_results.json}"
