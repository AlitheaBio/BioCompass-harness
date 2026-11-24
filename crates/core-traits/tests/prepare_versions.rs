use biocompass_core_traits::{
    PluginId, PrepareRunRequest, RunQuote, validate_requested_plugin_version,
};
use serde_json::json;

#[test]
fn prepare_preserves_exact_version_and_accepts_an_unpinned_legacy_request() {
    let legacy = json!({"org_id":"research", "plugin_id":"qa/echo", "params":{}});
    let mut explicit = legacy.clone();
    explicit["plugin_version"] = json!("1.0.0");
    let parsed: PrepareRunRequest = serde_json::from_value(explicit).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["plugin_version"],
        "1.0.0"
    );

    let parsed: PrepareRunRequest = serde_json::from_value(legacy).unwrap();
    assert!(
        serde_json::to_value(parsed)
            .unwrap()
            .get("plugin_version")
            .is_none()
    );
}

#[test]
fn exact_versions_reject_blank_and_keep_existing_offer_normalization() {
    let plugin = PluginId::new("qa/echo").unwrap();
    let quote: RunQuote = serde_json::from_value(json!({
        "run_id":"r", "plugin_id":"qa/echo", "plugin_version":"v2", "offer_id":"small",
        "image_ref":"fixture", "target_node_id":"worker", "estimated_cost_milli_eur":0,
        "workload_hash":"hash", "froglet_quote":{}, "expires_at":"2026-11-19T00:00:00Z"
    }))
    .unwrap();
    for selector in [None, Some("v2"), Some("0.1.0+local")] {
        assert!(validate_requested_plugin_version(selector).is_ok());
    }
    for selector in ["", "  ", "v2 ", " v2"] {
        assert!(validate_requested_plugin_version(Some(selector)).is_err());
    }
    for offer in [None, Some(""), Some("   "), Some(" small ")] {
        assert!(quote.validate_selection(&plugin, Some("v2"), offer).is_ok());
    }
    assert!(
        quote
            .validate_selection(&plugin, None, Some("small"))
            .is_ok()
    );
    assert!(quote.validate_selection(&plugin, Some("v1"), None).is_err());
    assert!(
        quote
            .validate_selection(&plugin, None, Some("large"))
            .is_err()
    );
    assert!(
        quote
            .validate_selection(&PluginId::new("qa/another").unwrap(), None, None)
            .is_err()
    );
}
