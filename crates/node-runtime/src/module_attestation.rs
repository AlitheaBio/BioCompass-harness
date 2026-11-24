//! Signed module attestations (`U-1`): a hub signs, for every module version it lists, the UI
//! bundle hash and size, the container image digest and its own key, using the same `froglet/v1`
//! RFC 8785 JCS + BIP340 Schnorr artifact signing as the run chain. A workspace pins the hub key
//! and verifies the attestation (and, for federated modules, the providing peer's signed offer)
//! before it caches or serves any UI bundle.

use froglet_protocol::crypto::{NodeSigningKey, public_key_hex, sign_message_hex};
use froglet_protocol::protocol::{SignedArtifact, sign_artifact, verify_artifact};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `artifact_type` of a hub-signed module attestation.
pub const MODULE_ATTESTATION_ARTIFACT_TYPE: &str = "bc.module_attestation.v1";

/// Offer capability carrying a UI bundle hash inside a signed `froglet/v1` offer.
pub const UI_BUNDLE_HASH_CAPABILITY_PREFIX: &str = "biocompass.ui_bundle_hash.v1:";

/// The connected hub's statement that a federated peer it approved provides this module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEndorsement {
    pub peer_id: String,
    /// The peer's signing key as recorded in the hub's approved federation peers.
    pub peer_pubkey: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_origin: Option<String>,
    /// Hash of the peer-signed `froglet/v1` offer that carries the UI bundle hash.
    pub offer_hash: String,
}

/// Signed content of a module attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleAttestationPayload {
    pub plugin_id: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_bundle_size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// The issuing hub's public key (always equal to the artifact signer).
    pub hub_key: String,
    pub issued_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderEndorsement>,
}

/// Why an attestation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestationError {
    Malformed(String),
    BadSignature,
    /// Signed by a key other than the pinned hub key.
    UntrustedSigner { pinned: String, presented: String },
    /// Attested values differ from what is being opened.
    Mismatch(String),
    /// The federated peer's signed offer failed verification.
    ProviderOffer(String),
}

impl std::fmt::Display for AttestationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(m) => write!(f, "malformed module attestation: {m}"),
            Self::BadSignature => write!(f, "module attestation signature is invalid"),
            Self::UntrustedSigner { pinned, presented } => write!(
                f,
                "module attestation signed by '{presented}', but the pinned hub key is '{pinned}'"
            ),
            Self::Mismatch(m) => write!(f, "module attestation does not match: {m}"),
            Self::ProviderOffer(m) => write!(f, "provider offer verification failed: {m}"),
        }
    }
}

impl std::error::Error for AttestationError {}

/// Sign `payload` with the hub key. `payload.hub_key` is overwritten with the signer's key.
pub fn sign_module_attestation(
    key: &NodeSigningKey,
    mut payload: ModuleAttestationPayload,
) -> Result<Value, String> {
    let signer = public_key_hex(key);
    payload.hub_key = signer.clone();
    let created_at = payload.issued_at;
    let signed = sign_artifact(
        &signer,
        |msg| sign_message_hex(key, msg),
        MODULE_ATTESTATION_ARTIFACT_TYPE,
        created_at,
        payload,
    )?;
    serde_json::to_value(signed).map_err(|e| e.to_string())
}

/// Verify a hub-signed attestation document against the pinned hub key and return its payload.
pub fn verify_module_attestation(
    doc: &Value,
    pinned_hub_key: &str,
) -> Result<ModuleAttestationPayload, AttestationError> {
    let signed: SignedArtifact<ModuleAttestationPayload> = serde_json::from_value(doc.clone())
        .map_err(|e| AttestationError::Malformed(e.to_string()))?;
    if signed.artifact_type != MODULE_ATTESTATION_ARTIFACT_TYPE {
        return Err(AttestationError::Malformed(format!(
            "unexpected artifact_type '{}'",
            signed.artifact_type
        )));
    }
    if !verify_artifact(&signed) {
        return Err(AttestationError::BadSignature);
    }
    let pinned = pinned_hub_key.trim().to_ascii_lowercase();
    let signer = signed.signer.to_ascii_lowercase();
    if signer != pinned {
        return Err(AttestationError::UntrustedSigner {
            pinned,
            presented: signer,
        });
    }
    if signed.payload.hub_key.to_ascii_lowercase() != signer {
        return Err(AttestationError::Mismatch(
            "payload hub_key differs from the signer".into(),
        ));
    }
    Ok(signed.payload)
}

/// Return the signer key of an attestation document without verifying it (for pinning UX).
pub fn attestation_signer(doc: &Value) -> Option<String> {
    doc.get("signer")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
}

/// For a federated module: verify the peer-signed `froglet/v1` offer against the endorsement in a
/// verified hub attestation, and that the offer signs the same UI bundle hash.
pub fn verify_provider_offer(
    attested: &ModuleAttestationPayload,
    offer_doc: &Value,
) -> Result<(), AttestationError> {
    let Some(provider) = &attested.provider else {
        return Err(AttestationError::ProviderOffer(
            "attestation carries no provider endorsement".into(),
        ));
    };
    let offer: SignedArtifact<Value> = serde_json::from_value(offer_doc.clone())
        .map_err(|e| AttestationError::ProviderOffer(format!("decode offer: {e}")))?;
    if offer.artifact_type != "offer" {
        return Err(AttestationError::ProviderOffer(format!(
            "expected an offer, found '{}'",
            offer.artifact_type
        )));
    }
    if !verify_artifact(&offer) {
        return Err(AttestationError::ProviderOffer(
            "offer signature is invalid".into(),
        ));
    }
    if !offer.signer.eq_ignore_ascii_case(&provider.peer_pubkey) {
        return Err(AttestationError::ProviderOffer(format!(
            "offer signed by '{}', but the approved peer key is '{}'",
            offer.signer, provider.peer_pubkey
        )));
    }
    if offer.hash != provider.offer_hash {
        return Err(AttestationError::ProviderOffer(
            "offer hash differs from the endorsed offer".into(),
        ));
    }
    let offered_hash = offer_ui_bundle_hash(&offer.payload);
    if offered_hash.as_deref() != attested.ui_bundle_hash.as_deref() {
        return Err(AttestationError::ProviderOffer(format!(
            "peer-signed UI bundle hash {offered_hash:?} differs from attested {:?}",
            attested.ui_bundle_hash
        )));
    }
    Ok(())
}

/// Extract the UI bundle hash from a signed offer payload's capabilities.
pub fn offer_ui_bundle_hash(offer_payload: &Value) -> Option<String> {
    offer_payload
        .pointer("/execution_profile/capabilities")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|cap| cap.strip_prefix(UI_BUNDLE_HASH_CAPABILITY_PREFIX))
        .map(|h| h.trim().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use froglet_protocol::crypto::generate_signing_key;
    use serde_json::json;

    fn payload(hash: &str) -> ModuleAttestationPayload {
        ModuleAttestationPayload {
            plugin_id: "alithea/demo".into(),
            version: "1.0.0".into(),
            ui_bundle_hash: Some(hash.into()),
            ui_bundle_size_bytes: Some(10),
            image_digest: Some(format!("sha256:{}", "1".repeat(64))),
            hub_key: String::new(),
            issued_at: 1_760_000_000,
            provider: None,
        }
    }

    #[test]
    fn attestation_verifies_only_against_the_pinned_key_and_detects_tampering() {
        let hub = generate_signing_key();
        let other = generate_signing_key();
        let doc = sign_module_attestation(&hub, payload(&"a".repeat(64))).unwrap();
        let ok = verify_module_attestation(&doc, &public_key_hex(&hub)).unwrap();
        assert_eq!(ok.ui_bundle_hash.as_deref(), Some("a".repeat(64).as_str()));
        assert!(matches!(
            verify_module_attestation(&doc, &public_key_hex(&other)),
            Err(AttestationError::UntrustedSigner { .. })
        ));
        let mut tampered = doc.clone();
        tampered["payload"]["ui_bundle_hash"] = json!("b".repeat(64));
        assert_eq!(
            verify_module_attestation(&tampered, &public_key_hex(&hub)),
            Err(AttestationError::BadSignature)
        );
    }

    #[test]
    fn provider_offer_must_be_signed_by_the_endorsed_peer_and_carry_the_same_hash() {
        let hub = generate_signing_key();
        let peer = generate_signing_key();
        let hash = "c".repeat(64);
        let offer = sign_artifact(
            &public_key_hex(&peer),
            |m| sign_message_hex(&peer, m),
            "offer",
            1_760_000_000,
            json!({"execution_profile": {"capabilities": [format!("{UI_BUNDLE_HASH_CAPABILITY_PREFIX}{hash}")]}}),
        )
        .unwrap();
        let offer_doc = serde_json::to_value(&offer).unwrap();
        let mut p = payload(&hash);
        p.provider = Some(ProviderEndorsement {
            peer_id: "peer-b".into(),
            peer_pubkey: public_key_hex(&peer),
            peer_origin: Some("https://b.example".into()),
            offer_hash: offer.hash.clone(),
        });
        let doc = sign_module_attestation(&hub, p).unwrap();
        let attested = verify_module_attestation(&doc, &public_key_hex(&hub)).unwrap();
        verify_provider_offer(&attested, &offer_doc).unwrap();

        // An offer re-signed by a different key (e.g. the connected hub itself) is refused.
        let impostor = sign_artifact(
            &public_key_hex(&hub),
            |m| sign_message_hex(&hub, m),
            "offer",
            1_760_000_000,
            offer.payload.clone(),
        )
        .unwrap();
        assert!(verify_provider_offer(&attested, &serde_json::to_value(impostor).unwrap()).is_err());
        // A tampered offer hash is refused.
        let mut bad = offer_doc.clone();
        bad["payload"]["execution_profile"]["capabilities"][0] =
            json!(format!("{UI_BUNDLE_HASH_CAPABILITY_PREFIX}{}", "d".repeat(64)));
        assert!(verify_provider_offer(&attested, &bad).is_err());
    }

    /// Cross-check: the test-only JS signer used by Playwright mock hubs
    /// (`e2e/support/froglet_sign.ts`, seed 0x11..11) produces documents this verifier accepts.
    #[test]
    fn js_test_signer_vector_verifies() {
        // Public test-vector key derived from the fixed seed [0x11; 32]; allowlisted in .gitleaks.toml.
        let doc: Value = serde_json::from_str(r#"{"artifact_type":"bc.module_attestation.v1","schema_version":"froglet/v1","signer":"4f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aa","created_at":1760000000,"payload_hash":"a1991cd9274c32c28f389aad6593ec2c53b9ef0bc3b796c34084957503065612","hash":"01c6b2a429cbb8dd1cc66b814d25d4c818a4c7949cee0836ec2cb171055dc948","payload":{"plugin_id":"org/mod","version":"1.0.0","ui_bundle_hash":"abababababababababababababababababababababababababababababababab","hub_key":"4f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aa","issued_at":1760000000},"signature":"6ee0e9eedc0a290c255aaa7f4fd2fb90fbff5c46722c07998546132ca9799d3dac69a945920db99c755a3fde986c081675b6f3951bebd3fee26e88db1e6a69ca"}"#).unwrap();
        let key = froglet_protocol::crypto::signing_key_from_seed_bytes(&[0x11; 32]).unwrap();
        assert_eq!(public_key_hex(&key), doc["signer"]);
        let p = verify_module_attestation(&doc, doc["signer"].as_str().unwrap()).unwrap();
        assert_eq!(p.plugin_id, "org/mod");
    }
}
