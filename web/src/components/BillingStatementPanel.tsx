import React, { useCallback, useEffect, useState } from "react";
import { fetchBillingStatement, formatErrorInline } from "../api";
import { formatMilliAct, type BillingStatement } from "../types";

/**
 * Milestone C1 (H4 / H3) — EUR Billing Statement with breakdowns by module, dataset, and
 * agent credential, plus itemized runs & receipt hashes.
 */
export function BillingStatementPanel() {
  const [statement, setStatement] = useState<BillingStatement | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [fromDate, setFromDate] = useState("");
  const [toDate, setToDate] = useState("");
  const [credentialFilter, setCredentialFilter] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await fetchBillingStatement({
        ...(fromDate ? { from: fromDate } : {}),
        ...(toDate ? { to: toDate } : {}),
        ...(credentialFilter.trim() ? { credential_id: credentialFilter.trim() } : {}),
      });
      setStatement(data);
    } catch (err: unknown) {
      setError(formatErrorInline(err));
    } finally {
      setLoading(false);
    }
  }, [fromDate, toDate, credentialFilter]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="bc-settings-stack" data-testid="billing-statement-panel">
      <div className="bc-card">
        <div className="bc-card-header">
          <div>
            <span className="bc-card-title">EUR Usage Statement</span>
            <p className="bc-caption" style={{ marginTop: 2, marginBottom: 0 }}>
              Settled module &amp; dataset fees in euros (1 ACT = €1.00) with verifiable receipt hashes.
            </p>
          </div>
          <button
            type="button"
            className="bc-btn bc-btn-secondary bc-btn-sm"
            disabled={loading}
            onClick={() => void load()}
          >
            {loading ? "Loading…" : "Refresh Statement"}
          </button>
        </div>

        <div
          style={{
            marginTop: 12,
            display: "grid",
            gridTemplateColumns: "repeat(auto-fit, minmax(160px, 1fr))",
            gap: 8,
          }}
        >
          <label className="bc-field">
            <span className="bc-field-label">From (ISO date)</span>
            <input
              type="date"
              className="bc-input mono"
              value={fromDate}
              onChange={(e) => setFromDate(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">To (ISO date)</span>
            <input
              type="date"
              className="bc-input mono"
              value={toDate}
              onChange={(e) => setToDate(e.target.value)}
            />
          </label>
          <label className="bc-field">
            <span className="bc-field-label">Agent credential ID (optional)</span>
            <input
              type="text"
              className="bc-input mono"
              placeholder="cred-..."
              value={credentialFilter}
              onChange={(e) => setCredentialFilter(e.target.value)}
            />
          </label>
        </div>

        {error && (
          <p role="alert" className="bc-caption" style={{ color: "var(--bc-danger)", marginTop: 10 }}>
            {error}
          </p>
        )}

        {statement && (
          <div className="bc-meta-grid" style={{ marginTop: 14 }} data-testid="statement-totals-grid">
            <div className="bc-meta-item">
              <span className="bc-caption">Total Spend (EUR)</span>
              <strong className="mono" style={{ fontSize: 18 }}>
                {formatMilliAct(statement.totals.total_milli_eur)}
              </strong>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Module Fees</span>
              <strong className="mono">{formatMilliAct(statement.totals.module_fee_milli_eur)}</strong>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Dataset Fees</span>
              <strong className="mono">{formatMilliAct(statement.totals.dataset_fee_milli_eur)}</strong>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Refunded</span>
              <strong className="mono">{formatMilliAct(statement.totals.refunded_milli_eur)}</strong>
            </div>
            <div className="bc-meta-item">
              <span className="bc-caption">Runs Settled</span>
              <strong className="mono">{statement.totals.run_count}</strong>
            </div>
          </div>
        )}
      </div>

      {statement && (
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(260px, 1fr))", gap: 12 }}>
          <section className="bc-card">
            <div className="bc-card-title" style={{ marginBottom: 8 }}>By Module</div>
            {statement.by_module.length === 0 ? (
              <p className="bc-empty-note">No module charges in window.</p>
            ) : (
              <table className="data-table">
                <thead>
                  <tr><th>Module</th><th>Runs</th><th style={{ textAlign: "right" }}>EUR</th></tr>
                </thead>
                <tbody>
                  {statement.by_module.map((g) => (
                    <tr key={g.plugin_id}>
                      <td className="mono">{g.plugin_id}</td>
                      <td className="mono">{g.run_count}</td>
                      <td className="mono" style={{ textAlign: "right" }}>{formatMilliAct(g.total_milli_eur)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <section className="bc-card">
            <div className="bc-card-title" style={{ marginBottom: 8 }}>By Dataset</div>
            {statement.by_dataset.length === 0 ? (
              <p className="bc-empty-note">No dataset charges in window.</p>
            ) : (
              <table className="data-table">
                <thead>
                  <tr><th>Dataset</th><th>Runs</th><th style={{ textAlign: "right" }}>EUR</th></tr>
                </thead>
                <tbody>
                  {statement.by_dataset.map((g) => (
                    <tr key={g.catalog_id}>
                      <td className="mono">{g.catalog_id}</td>
                      <td className="mono">{g.run_count}</td>
                      <td className="mono" style={{ textAlign: "right" }}>{formatMilliAct(g.dataset_fee_milli_eur)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <section className="bc-card">
            <div className="bc-card-title" style={{ marginBottom: 8 }}>By Agent Credential</div>
            {statement.by_credential.length === 0 ? (
              <p className="bc-empty-note">No credential-attributed runs in window.</p>
            ) : (
              <table className="data-table">
                <thead>
                  <tr><th>Credential</th><th>Runs</th><th style={{ textAlign: "right" }}>EUR</th></tr>
                </thead>
                <tbody>
                  {statement.by_credential.map((g) => (
                    <tr key={g.credential_id ?? g.label}>
                      <td className="mono">{g.label}</td>
                      <td className="mono">{g.run_count}</td>
                      <td className="mono" style={{ textAlign: "right" }}>{formatMilliAct(g.total_milli_eur)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>
        </div>
      )}

      {statement && statement.entries.length > 0 && (
        <section className="bc-card">
          <div className="bc-card-title" style={{ marginBottom: 8 }}>Itemized Runs</div>
          <div className="table-scroll">
            <table className="data-table">
              <thead>
                <tr>
                  <th>Run ID</th>
                  <th>Module</th>
                  <th>Credential</th>
                  <th>Settled / Fees</th>
                  <th>Receipt Hash</th>
                  <th>Timestamp</th>
                </tr>
              </thead>
              <tbody>
                {statement.entries.map((entry) => (
                  <tr key={entry.run_id}>
                    <td className="mono">{entry.run_id.slice(0, 14)}</td>
                    <td className="mono">{entry.plugin_id}</td>
                    <td className="mono">
                      {entry.agent_credential_label ?? entry.agent_credential_id ?? "human"}
                    </td>
                    <td className="mono">
                      {formatMilliAct(entry.settled_milli_eur)} ({formatMilliAct(entry.module_fee_milli_eur)} + {formatMilliAct(entry.dataset_fee_milli_eur)})
                    </td>
                    <td className="mono">{entry.receipt_hash ? `${entry.receipt_hash.slice(0, 16)}…` : "—"}</td>
                    <td className="bc-caption mono">{new Date(entry.created_at).toLocaleString()}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}
    </div>
  );
}
