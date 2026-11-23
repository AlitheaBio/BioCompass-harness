import React, { useCallback, useEffect, useState } from "react";
import {
  createWebhook,
  deleteWebhook,
  fetchWebhookDeliveries,
  fetchWebhooks,
  formatErrorInline,
  updateWebhook,
} from "../api";
import type {
  CreatedWebhookEndpoint,
  WebhookDeliveryRecord,
  WebhookEndpointRecord,
} from "../types";
import { IconCheck, IconCopy, IconTrash } from "./Icons";

const ALL_WEBHOOK_EVENTS = ["run.succeeded", "run.failed", "run.canceled"] as const;

/**
 * Milestone C1 (H4) — Outbound HMAC-SHA256 Webhooks CRUD & Delivery Log Inspector.
 */
export function WebhooksPanel() {
  const [endpoints, setEndpoints] = useState<WebhookEndpointRecord[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [deliveries, setDeliveries] = useState<WebhookDeliveryRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<CreatedWebhookEndpoint | null>(null);
  const [copiedSecret, setCopiedSecret] = useState(false);

  const [label, setLabel] = useState("");
  const [url, setUrl] = useState("");
  const [events, setEvents] = useState<string[]>([...ALL_WEBHOOK_EVENTS]);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setEndpoints(await fetchWebhooks());
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const loadDeliveries = useCallback(async (endpointId: string) => {
    setSelectedId(endpointId);
    try {
      setDeliveries(await fetchWebhookDeliveries(endpointId));
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    }
  }, []);

  const toggleEvent = (ev: string) => {
    setEvents((prev) => (prev.includes(ev) ? prev.filter((x) => x !== ev) : [...prev, ev]));
  };

  const handleCreate = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!url.trim() || events.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const res = await createWebhook({
        label: label.trim() || "Run Event Webhook",
        url: url.trim(),
        event_types: events,
      });
      setCreated(res);
      setLabel("");
      setUrl("");
      await refresh();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleToggleEnabled = async (ep: WebhookEndpointRecord) => {
    setBusy(true);
    setError(null);
    try {
      await updateWebhook(ep.endpoint_id, { enabled: !ep.enabled });
      await refresh();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const handleDelete = async (endpointId: string) => {
    setBusy(true);
    setError(null);
    try {
      await deleteWebhook(endpointId);
      if (selectedId === endpointId) {
        setSelectedId(null);
        setDeliveries([]);
      }
      await refresh();
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  };

  const createdSecret = created?.signing_secret ?? created?.secret ?? null;

  return (
    <div className="bc-settings-stack" data-testid="webhooks-panel">
      {error && (
        <div role="alert" className="bc-inline-alert bc-inline-alert-error">
          {error}
        </div>
      )}

      {createdSecret && (
        <div className="bc-card" style={{ border: "1px solid var(--bc-accent)" }} data-testid="webhook-secret-banner">
          <div className="bc-card-header">
            <span className="bc-card-title">HMAC-SHA256 Signing Secret (shown once)</span>
            <button
              type="button"
              className="bc-btn bc-btn-secondary bc-btn-sm"
              onClick={() => {
                void navigator.clipboard?.writeText(createdSecret);
                setCopiedSecret(true);
                window.setTimeout(() => setCopiedSecret(false), 1600);
              }}
            >
              {copiedSecret ? <IconCheck size={13} /> : <IconCopy size={13} />}
              <span>{copiedSecret ? "Copied" : "Copy secret"}</span>
            </button>
          </div>
          <p className="bc-caption" style={{ marginTop: 4 }}>
            Verify deliveries via <code>X-BioCompass-Signature: sha256=&lt;hmac-hex&gt;</code>.
          </p>
          <pre className="bc-code-pre mono">{createdSecret}</pre>
        </div>
      )}

      <section className="bc-card">
        <div className="bc-card-header">
          <span className="bc-card-title">Register Outbound Webhook</span>
          <span className="bc-caption">Transactional outbox · SSRF-guarded HTTPS destinations</span>
        </div>
        <form className="bc-form-grid" style={{ marginTop: 10 }} onSubmit={(e) => void handleCreate(e)}>
          <label className="bc-field">
            <span className="bc-field-label">Label</span>
            <input
              className="bc-input"
              data-testid="webhook-label-input"
              placeholder="LIMS / Agent Orchestrator"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Destination URL</span>
            <input
              type="url"
              className="bc-input mono"
              data-testid="webhook-url-input"
              placeholder="https://hooks.customer.example/biocompass"
              required
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          </label>
          <div className="bc-field" style={{ gridColumn: "1 / -1" }}>
            <span className="bc-field-label">Subscribed events</span>
            <div style={{ display: "flex", gap: 12, flexWrap: "wrap", marginTop: 4 }}>
              {ALL_WEBHOOK_EVENTS.map((ev) => (
                <label key={ev} style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                  <input
                    type="checkbox"
                    checked={events.includes(ev)}
                    onChange={() => toggleEvent(ev)}
                  />
                  <span className="mono">{ev}</span>
                </label>
              ))}
            </div>
          </div>
          <div>
            <button
              type="submit"
              className="bc-btn bc-btn-primary"
              data-testid="webhook-create-btn"
              disabled={busy || !url.trim() || events.length === 0}
            >
              {busy ? "Creating…" : "Add Webhook Endpoint"}
            </button>
          </div>
        </form>
      </section>

      <section className="bc-card">
        <div className="bc-card-header">
          <span className="bc-card-title">Configured Webhooks</span>
          <button type="button" className="bc-btn bc-btn-ghost bc-btn-sm" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
        {loading ? (
          <p className="bc-empty-note">Loading webhooks…</p>
        ) : endpoints.length === 0 ? (
          <p className="bc-empty-note">No outbound webhooks configured.</p>
        ) : (
          <div className="table-scroll">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Label &amp; URL</th>
                  <th>Events</th>
                  <th>Status</th>
                  <th>Actions</th>
                </tr>
              </thead>
              <tbody>
                {endpoints.map((ep) => (
                  <tr key={ep.endpoint_id} data-testid={`webhook-row-${ep.endpoint_id}`}>
                    <td>
                      <strong>{ep.label || ep.endpoint_id}</strong>
                      <div className="bc-caption mono">{ep.url}</div>
                    </td>
                    <td className="mono bc-caption">{ep.event_types.join(", ")}</td>
                    <td>
                      <span className={`bc-badge ${ep.enabled ? "bc-badge-success" : "bc-badge-neutral"}`}>
                        {ep.enabled ? "Enabled" : "Disabled"}
                      </span>
                    </td>
                    <td>
                      <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                        <button
                          type="button"
                          className="bc-btn bc-btn-secondary bc-btn-sm"
                          onClick={() => void loadDeliveries(ep.endpoint_id)}
                        >
                          Deliveries
                        </button>
                        <button
                          type="button"
                          className="bc-btn bc-btn-ghost bc-btn-sm"
                          disabled={busy}
                          onClick={() => void handleToggleEnabled(ep)}
                        >
                          {ep.enabled ? "Disable" : "Enable"}
                        </button>
                        <button
                          type="button"
                          className="bc-btn bc-btn-ghost bc-btn-sm"
                          disabled={busy}
                          onClick={() => void handleDelete(ep.endpoint_id)}
                        >
                          <IconTrash size={13} />
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {selectedId && (
        <section className="bc-card" data-testid="webhook-deliveries-panel">
          <div className="bc-card-header">
            <span className="bc-card-title">Delivery Log ({selectedId})</span>
            <button
              type="button"
              className="bc-btn bc-btn-ghost bc-btn-sm"
              onClick={() => void loadDeliveries(selectedId)}
            >
              Reload Logs
            </button>
          </div>
          {deliveries.length === 0 ? (
            <p className="bc-empty-note">No delivery attempts recorded yet.</p>
          ) : (
            <div className="table-scroll">
              <table className="data-table">
                <thead>
                  <tr>
                    <th>Event</th>
                    <th>Status</th>
                    <th>Attempts</th>
                    <th>HTTP</th>
                    <th>Last Error</th>
                    <th>Timestamp</th>
                  </tr>
                </thead>
                <tbody>
                  {deliveries.map((d) => (
                    <tr key={d.delivery_id}>
                      <td className="mono">{d.event_type}</td>
                      <td>
                        <span
                          className={`bc-badge ${
                            d.status === "delivered"
                              ? "bc-badge-success"
                              : d.status === "pending"
                                ? "bc-badge-neutral"
                                : "bc-badge-danger"
                          }`}
                        >
                          {d.status}
                        </span>
                      </td>
                      <td className="mono">
                        {d.attempt_count}/{d.max_attempts}
                      </td>
                      <td className="mono">{d.last_http_status ?? "—"}</td>
                      <td className="bc-caption">{d.last_error ?? "—"}</td>
                      <td className="bc-caption mono">
                        {new Date(d.delivered_at ?? d.updated_at).toLocaleString()}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}
    </div>
  );
}
