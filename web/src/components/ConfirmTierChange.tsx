import { useState } from "react";
import { confirmDemoTier, formatErrorInline } from "../api";
import type { TierDefinitionRecord } from "../types";

interface Props {
  tiers: TierDefinitionRecord[];
  onConfirmed: (message: string) => void;
}

export function ConfirmTierChange({ tiers, onConfirmed }: Props) {
  const [orgId, setOrgId] = useState("");
  const [tierId, setTierId] = useState("");
  const [reviewing, setReviewing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const eligible = tiers.filter(t => t.tier_id !== "demo" && t.tier_id !== "platform");
  const target = eligible.find(t => t.tier_id === tierId);

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    if (!orgId.trim() || !target || busy) return;
    setError("");
    onConfirmed("");
    if (!reviewing) {
      setReviewing(true);
      return;
    }
    setBusy(true);
    try {
      const converted = await confirmDemoTier(orgId.trim(), tierId);
      onConfirmed(converted.changed
        ? `Converted ${converted.org_id} from demo to ${converted.tier_id}.`
        : `${converted.org_id} is already on ${converted.tier_id}.`);
      setOrgId("");
      setTierId("");
      setReviewing(false);
    } catch (err) {
      setError(formatErrorInline(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="bc-card">
      <h2 className="bc-card-title">Convert a demo organization</h2>
      <form className="bc-form-grid" onSubmit={submit}>
        <label className="bc-field">
          <span className="bc-field-label">Demo organization ID</span>
          <input className="bc-input mono" data-testid="ct-convert-org-id"
            required disabled={reviewing} value={orgId} onChange={e => setOrgId(e.target.value)} />
        </label>
        <label className="bc-field">
          <span className="bc-field-label">Paid tier</span>
          <select className="bc-input" data-testid="ct-convert-tier" required disabled={reviewing}
            value={tierId} onChange={e => setTierId(e.target.value)}>
            <option value="">Select a tier</option>
            {eligible.map(t => <option key={t.tier_id} value={t.tier_id}>{t.name} ({t.tier_id})</option>)}
          </select>
        </label>
        {reviewing && target && (
          <p style={{ gridColumn: "1 / -1" }}>
            Apply the saved {target.name} tier policy to <strong>{orgId.trim()}</strong>?
          </p>
        )}
        {error && <p role="alert" style={{ gridColumn: "1 / -1" }}>{error}</p>}
        <div style={{ gridColumn: "1 / -1", display: "flex", gap: 8 }}>
          {reviewing && (
            <button className="bc-btn" type="button" disabled={busy}
              onClick={() => { setReviewing(false); setError(""); }}>
              Back
            </button>
          )}
          <button className="bc-btn bc-btn-primary" type="submit"
            data-testid={reviewing ? "ct-confirm-conversion" : "ct-review-conversion"}
            disabled={busy || !orgId.trim() || !target}>
            {busy ? "Converting…" : reviewing ? "Confirm tier change" : "Review tier change"}
          </button>
        </div>
      </form>
    </section>
  );
}
