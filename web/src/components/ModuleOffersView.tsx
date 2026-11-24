import { useCallback, useEffect, useState, type FormEvent } from "react";
import {
  fetchModuleOffers,
  fetchOrgPluginVersions,
  formatErrorInline,
  offeredModuleDownloadUrl,
  publishModuleOffer,
  subscribeModuleOffer,
} from "../api";
import {
  hasMinOrgRole,
  type AuthPrincipal,
  type ModuleOffer,
  type ModuleOfferAction,
  type ModuleOfferScope,
  type PluginVersionRecord,
} from "../types";

const scopeLabels: Record<ModuleOfferScope, string> = {
  private: "Private",
  custom_shared: "Custom shared",
  global: "Global",
};

function releaseKey(release: PluginVersionRecord): string {
  return `${release.plugin_id}@${release.version}`;
}

export function ModuleOffersView({
  principal,
  onOpenRun,
}: {
  principal: AuthPrincipal;
  onOpenRun: (offer: ModuleOffer) => Promise<void>;
}) {
  const canRequest = hasMinOrgRole(principal.role, "member");
  const canPublish = hasMinOrgRole(principal.role, "developer");
  const [offers, setOffers] = useState<ModuleOffer[]>([]);
  const [releases, setReleases] = useState<PluginVersionRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [releaseId, setReleaseId] = useState("");
  const [title, setTitle] = useState("");
  const [action, setAction] = useState<ModuleOfferAction>("run");
  const [scope, setScope] = useState<ModuleOfferScope>("private");
  const [recipients, setRecipients] = useState("");

  const selectedRelease = releases.find((item) => releaseKey(item) === releaseId);
  const canShare = selectedRelease?.visibility === "public_approved";

  const refresh = useCallback(async () => {
    const listed = await fetchModuleOffers();
    setOffers(listed);
  }, []);

  useEffect(() => {
    void refresh().catch((reason: unknown) => setError(formatErrorInline(reason)))
      .finally(() => setLoading(false));
  }, [refresh]);

  useEffect(() => {
    if (!canPublish) return;
    void fetchOrgPluginVersions()
      .then((versions) => setReleases(versions.filter((release) =>
        release.org_id === principal.org_id && release.visibility !== "rejected")))
      .catch((reason: unknown) => setError(formatErrorInline(reason)));
  }, [canPublish, principal.org_id]);

  async function perform(operation: () => Promise<void>) {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await operation();
    } catch (reason: unknown) {
      setError(formatErrorInline(reason));
    } finally {
      setBusy(false);
    }
  }

  async function publish(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!selectedRelease || !title.trim()) return;
    await perform(async () => {
      const parsedRecipients = scope === "custom_shared" ? recipients.split(/[\s,]+/).filter(Boolean) : [];
      await publishModuleOffer({
        plugin_id: selectedRelease.plugin_id,
        version: selectedRelease.version,
        title: title.trim(),
        action,
        scope,
        allowed_orgs: parsedRecipients,
        approval_required: false,
      });
      setTitle("");
      setMessage("Module offer published.");
      await refresh();
    });
  }

  function offerControls(offer: ModuleOffer) {
    if (offer.access !== "granted") {
      return canRequest ? (
        <button type="button" className="bc-btn bc-btn-secondary" disabled={busy} onClick={() => {
          void perform(async () => {
            await subscribeModuleOffer(offer.offer_id);
            setMessage("Subscribed to module offer.");
            await refresh();
          });
        }}>Subscribe</button>
      ) : "Subscription required";
    }
    if (offer.action === "run") {
      return canRequest ? (
        <button type="button" className="bc-btn bc-btn-secondary" disabled={busy}
          onClick={() => void perform(() => onOpenRun(offer))}>Open</button>
      ) : "Run access granted";
    }

    const filename = `${offer.plugin_id.replace("/", "-")}-${offer.version}`;
    return (
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <a className="bc-btn bc-btn-secondary" href={offeredModuleDownloadUrl(offer, "package")}
          download={`${filename}-package.json`}>Download package</a>
        <a className="bc-btn bc-btn-secondary" href={offeredModuleDownloadUrl(offer, "image")}
          download={`${filename}-image.tar`}>Download image</a>
      </div>
    );
  }

  return (
    <div className="bc-settings-stack">
      {error && <p role="alert" className="bc-error-banner">{error}</p>}
      {message && <p role="status" className="bc-caption">{message}</p>}

      <section className="bc-card" aria-label="Module marketplace">
        <div className="bc-card-header">
          <h2 className="bc-card-title">Module marketplace</h2>
          <button type="button" className="bc-btn bc-btn-ghost" disabled={busy || loading}
            onClick={() => void perform(refresh)}>Refresh offers</button>
        </div>
        {loading ? <p className="bc-empty-note">Loading offers…</p> : offers.length === 0 ? (
          <p className="bc-empty-note">No module offers are available for this organization.</p>
        ) : (
          <div className="table-scroll">
            <table className="data-table">
              <thead><tr><th>Offer</th><th>Module release</th><th>Scope</th><th>Access</th></tr></thead>
              <tbody>
                {offers.map((offer) => (
                  <tr key={offer.offer_id} data-testid={`module-offer-${offer.offer_id}`}>
                    <th scope="row">
                      {offer.title}
                      <div className="bc-caption">{offer.action === "run" ? "Hosted run" : "Download"}</div>
                    </th>
                    <td className="mono">{offer.plugin_id} · {offer.version}</td>
                    <td>{scopeLabels[offer.scope]}</td>
                    <td>{offerControls(offer)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {canPublish && (
        <section className="bc-card" aria-label="Publish module offer">
          <div className="bc-card-header"><h2 className="bc-card-title">Publish a module offer</h2></div>
          <form className="bc-form-grid" onSubmit={(event) => void publish(event)}>
            <label className="bc-field">
              <span className="bc-field-label">Module release</span>
              <select className="bc-input" required value={releaseId} onChange={(event) => {
                setReleaseId(event.target.value);
                setScope("private");
              }}>
                <option value="">Select a published release</option>
                {releases.map((release) => (
                  <option key={releaseKey(release)} value={releaseKey(release)}>
                    {release.version} · {release.name} ({release.plugin_id})
                  </option>
                ))}
              </select>
              {selectedRelease && <span className="bc-caption mono" style={{ overflowWrap: "anywhere" }}>
                {releaseKey(selectedRelease)}
              </span>}
            </label>
            <label className="bc-field">
              <span className="bc-field-label">Offer title</span>
              <input className="bc-input" maxLength={160} required value={title}
                onChange={(event) => setTitle(event.target.value)} />
            </label>
            <label className="bc-field">
              <span className="bc-field-label">Offer action</span>
              <select className="bc-input" value={action}
                onChange={(event) => setAction(event.target.value as ModuleOfferAction)}>
                <option value="run">Hosted run</option>
                <option value="download">Download</option>
              </select>
            </label>
            <label className="bc-field">
              <span className="bc-field-label">Offer visibility</span>
              <select className="bc-input" value={scope}
                onChange={(event) => setScope(event.target.value as ModuleOfferScope)}>
                <option value="private">Private</option>
                <option value="custom_shared" disabled={!canShare}>Custom shared</option>
                <option value="global" disabled={!canShare}>Global</option>
              </select>
              {selectedRelease && !canShare && <span className="bc-caption">Shared offers require release approval.</span>}
            </label>
            {scope === "custom_shared" && (
              <label className="bc-field">
                <span className="bc-field-label">Recipient organization IDs</span>
                <input className="bc-input" required value={recipients} placeholder="org-a, org-b"
                  onChange={(event) => setRecipients(event.target.value)} />
              </label>
            )}
            <div>
              <button type="submit" className="bc-btn bc-btn-primary"
                disabled={busy || !selectedRelease || !title.trim()}>Publish module offer</button>
            </div>
          </form>
        </section>
      )}
    </div>
  );
}
