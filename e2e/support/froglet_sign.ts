/**
 * Test-only `froglet/v1` artifact signer for mock hubs: RFC 8785 JCS + BIP340 Schnorr over
 * SHA-256 of the canonical signing bytes (as `froglet_protocol::sign_artifact` with k256 does).
 * Lets UI-flow specs serve hub-signed `bc.module_attestation.v1` documents (`U-1`).
 */
import { createHash } from "node:crypto";

const P = 0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffefffffc2fn;
const N = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
const G: [bigint, bigint] = [
  0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798n,
  0x483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8n,
];
type Pt = [bigint, bigint] | null;

const mod = (a: bigint, m = P): bigint => ((a % m) + m) % m;
function inv(a: bigint, m = P): bigint {
  let [lo, hi, x0, x1] = [mod(a, m), m, 1n, 0n];
  while (lo > 1n) {
    const q = hi / lo;
    [lo, hi] = [hi - q * lo, lo];
    [x0, x1] = [x1 - q * x0, x0];
  }
  return mod(x0, m);
}
function add(a: Pt, b: Pt): Pt {
  if (!a) return b;
  if (!b) return a;
  if (a[0] === b[0] && mod(a[1] + b[1]) === 0n) return null;
  const l = a[0] === b[0] && a[1] === b[1]
    ? mod(3n * a[0] * a[0] * inv(2n * a[1]))
    : mod((b[1] - a[1]) * inv(b[0] - a[0]));
  const x = mod(l * l - a[0] - b[0]);
  return [x, mod(l * (a[0] - x) - a[1])];
}
function mul(k: bigint, p: Pt = G): Pt {
  let r: Pt = null;
  let q = p;
  while (k > 0n) {
    if (k & 1n) r = add(r, q);
    q = add(q, q);
    k >>= 1n;
  }
  return r;
}
const hex32 = (n: bigint): string => n.toString(16).padStart(64, "0");
const big = (b: Buffer): bigint => BigInt(`0x${b.toString("hex") || "0"}`);
const sha = (...parts: Buffer[]): Buffer => {
  const h = createHash("sha256");
  for (const p of parts) h.update(p);
  return h.digest();
};
const tagged = (tag: string, ...parts: Buffer[]): Buffer => {
  const t = sha(Buffer.from(tag));
  return sha(t, t, ...parts);
};

export function publicKeyHex(seedHex: string): string {
  return hex32(mul(big(Buffer.from(seedHex, "hex")))![0]);
}

/** BIP340 signature of `msg32` (already hashed) with all-zero aux randomness. */
function schnorr(seedHex: string, msg32: Buffer): string {
  let d = big(Buffer.from(seedHex, "hex"));
  const pub = mul(d)!;
  if (pub[1] & 1n) d = N - d;
  const px = Buffer.from(hex32(pub[0]), "hex");
  const t = Buffer.from(hex32(d ^ big(tagged("BIP0340/aux", Buffer.alloc(32)))), "hex");
  let k = mod(big(tagged("BIP0340/nonce", t, px, msg32)), N);
  const R = mul(k)!;
  if (R[1] & 1n) k = N - k;
  const rx = Buffer.from(hex32(R[0]), "hex");
  const e = mod(big(tagged("BIP0340/challenge", rx, px, msg32)), N);
  return rx.toString("hex") + hex32(mod(k + e * d, N));
}

/** RFC 8785 for the JSON used here (strings, integers, booleans, arrays, objects). */
export function jcs(v: unknown): string {
  if (v === null || typeof v !== "object") return JSON.stringify(v);
  if (Array.isArray(v)) return `[${v.map(jcs).join(",")}]`;
  const o = v as Record<string, unknown>;
  return `{${Object.keys(o)
    .filter((k) => o[k] !== undefined)
    .sort()
    .map((k) => `${JSON.stringify(k)}:${jcs(o[k])}`)
    .join(",")}}`;
}

export function signArtifact(seedHex: string, artifactType: string, createdAt: number, payload: unknown): Record<string, unknown> {
  const signer = publicKeyHex(seedHex);
  const payloadHash = sha(Buffer.from(jcs(payload))).toString("hex");
  const signing = Buffer.from(jcs(["froglet/v1", artifactType, signer, createdAt, payloadHash, payload]));
  return {
    artifact_type: artifactType,
    schema_version: "froglet/v1",
    signer,
    created_at: createdAt,
    payload_hash: payloadHash,
    hash: sha(signing).toString("hex"),
    payload,
    signature: schnorr(seedHex, sha(signing)),
  };
}

/** Hub-signed `bc.module_attestation.v1` (omit absent optional fields, as the Rust struct does). */
export function moduleAttestation(
  seedHex: string,
  pluginId: string,
  version: string,
  uiBundleHash: string,
  provider?: { peer_id: string; peer_pubkey: string; peer_origin?: string; offer_hash: string },
): Record<string, unknown> {
  const payload: Record<string, unknown> = {
    plugin_id: pluginId,
    version,
    ui_bundle_hash: uiBundleHash,
    hub_key: publicKeyHex(seedHex),
    issued_at: 1_760_000_000,
  };
  if (provider) payload["provider"] = provider;
  return signArtifact(seedHex, "bc.module_attestation.v1", 1_760_000_000, payload);
}
