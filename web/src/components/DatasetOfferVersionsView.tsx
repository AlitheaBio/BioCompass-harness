import { useState } from "react";
import { addDatasetOfferVersion, fetchDatasetOfferVersions, formatErrorInline, type DatasetOfferVersions } from "../api";
import type { DatasetOfferRecord } from "../types";

export function DatasetOfferVersionsView({ offer, publisher }: { offer: DatasetOfferRecord; publisher: boolean }) {
  const [versions, setVersions] = useState<DatasetOfferVersions>();
  const [selected, setSelected] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function refresh(add = false) {
    setBusy(true);
    setError("");
    try {
      if (add) {
        const number = Number(selected);
        const release = versions?.available_releases?.find(candidate => candidate.release_version === number);
        await addDatasetOfferVersion(offer.offer_id, release ? { release_version: number } : { snapshot_id: number });
      }
      setVersions(await fetchDatasetOfferVersions(offer.offer_id));
      setSelected("");
    } catch (e) {
      setError(formatErrorInline(e));
    } finally {
      setBusy(false);
    }
  }
  const audience = offer.scope === "custom_shared" ? offer.allowed_orgs?.join(", ")
    : offer.scope === "private" ? offer.publisher_org_id : "all eligible organizations";
  const choices = versions?.available_releases?.length
    ? versions.available_releases.map(release => release.release_version)
    : versions?.available_snapshots ?? [];
  return <div>
    <button type="button" className="bc-btn bc-btn-sm" disabled={busy}
      onClick={() => void refresh()}>View releases</button>
    {versions && <div>
      <p>{versions.versions.length ? `Offered releases: ${versions.versions.map(v => v.release_version ?? v.snapshot_id).join(", ")}` : "No verified releases."}</p>
      {publisher && choices.length > 0 && <div>
        <label>Release to add <select value={selected} onChange={e => setSelected(e.target.value)}>
          <option value="">Select release</option>
          {choices.map(id => <option key={id} value={id}>{id}</option>)}
        </select></label>
        {selected && <p>Release {selected} will use this offer’s access policy for {audience}. Existing approvals apply.</p>}
        <button type="button" className="bc-btn bc-btn-primary bc-btn-sm" disabled={busy || selected === ""}
          onClick={() => void refresh(true)}>Add release</button>
      </div>}
    </div>}
    {error && <p role="alert">{error}</p>}
  </div>;
}
