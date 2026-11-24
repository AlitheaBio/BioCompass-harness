import { useCallback, useEffect, useState, type FormEvent } from "react";
import {
  approveModuleOffer,
  fetchModuleOfferRequests,
  fetchFederatedModuleOffers,
  fetchModuleOffers,
  fetchOrgPluginVersions,
  formatErrorInline,
  offeredModuleDownloadUrl,
  publishModuleOffer,
  subscribeModuleOffer,
  updateModuleOfferPrice,
} from "../api";
import { sanitizeFederatedLaunchUrl } from "../safeUrl";
import { formatFederatedOfferTarget } from "../milestoneBPolicy";
import {
  formatMilliAct,
  hasMinOrgRole,
  type AuthPrincipal,
  type FederatedModuleOffer,
  type ModuleOffer,
  type ModuleOfferAction,
  type ModuleOfferRequest,
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
  const canApprove = hasMinOrgRole(principal.role, "admin");
  const [offers, setOffers] = useState<ModuleOffer[]>([]);
  const [federated, setFederated] = useState<FederatedModuleOffer[]>([]);
  const [unavailable, setUnavailable] = useState<string[]>([]);
  const [requests, setRequests] = useState<ModuleOfferRequest[]>([]);
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
  const [requiresApproval, setRequiresApproval] = useState(false);
  const [manifestOfferId, setManifestOfferId] = useState("standard");
  const [negotiatedPriceEur, setNegotiatedPriceEur] = useState("");
  const [negotiatedBaseFeeEur, setNegotiatedBaseFeeEur] = useState("");
  const [editingPriceOfferId, setEditingPriceOfferId] = useState<string | null>(null);
  const [editPriceEur, setEditPriceEur] = useState("");
  const [editBaseFeeEur, setEditBaseFeeEur] = useState("0");
  const [editManifestOfferId, setEditManifestOfferId] = useState("standard");

  const selectedRelease = releases.find((item) => releaseKey(item) === releaseId);
  const canShare = selectedRelease?.visibility === "public_approved";

  const refresh = useCallback(async () => {
    const [listed, decisions, remote] = await Promise.all([
      fetchModuleOffers(),
      canRequest ? fetchModuleOfferRequests() : Promise.resolve([]),
      canRequest && principal.auth_method === "session"
        ? fetchFederatedModuleOffers()
        : Promise.resolve({ offers: [], unavailable: [] }),
    ]);
    setOffers(listed);
    setRequests(decisions);
    setFederated(remote.offers);
    setUnavailable(remote.unavailable);
  }, [canRequest, principal.auth_method]);

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
      const negotiatedPrice =
        scope === "custom_shared" && negotiatedPriceEur.trim() !== ""
          ? Math.round(Number(negotiatedPriceEur) * 1000)
          : undefined;
      const negotiatedBaseFee =
        scope === "custom_shared" && negotiatedBaseFeeEur.trim() !== ""
          ? Math.round(Number(negotiatedBaseFeeEur) * 1000)
          : undefined;
      await publishModuleOffer({
        plugin_id: selectedRelease.plugin_id,
        version: selectedRelease.version,
        title: title.trim(),
        action,
        scope,
        allowed_orgs: parsedRecipients,
        approval_required: scope !== "private" && requiresApproval,
        ...(negotiatedPrice != null && Number.isFinite(negotiatedPrice)
          ? {
              manifest_offer_id: manifestOfferId.trim() || "standard",
              price_milli_act: negotiatedPrice,
              base_fee_milli_act:
                negotiatedBaseFee != null && Number.isFinite(negotiatedBaseFee)
                  ? negotiatedBaseFee
                  : 0,
            }
          : {}),
      });
      setTitle("");
      setNegotiatedPriceEur("");
      setNegotiatedBaseFeeEur("");
      setMessage("Module offer published.");
      await refresh();
    });
  }

  async function saveNegotiatedPrice(offerId: string) {
    const priceMilli = Math.round(Number(editPriceEur) * 1000);
    const baseMilli = Math.round((Number(editBaseFeeEur) || 0) * 1000);
    if (!Number.isFinite(priceMilli) || priceMilli < 0) {
      setError("Enter a valid non-negative negotiated EUR price.");
      return;
    }
    await perform(async () => {
      await updateModuleOfferPrice(offerId, {
        manifest_offer_id: editManifestOfferId.trim() || "standard",
        price_milli_act: priceMilli,
        base_fee_milli_act: Math.max(0, baseMilli),
      });
      setEditingPriceOfferId(null);
      setMessage("Negotiated EUR rate updated.");
      await refresh();
    });
  }

  function offerControls(offer: ModuleOffer) {
    if (offer.access === "tier_restricted") return "Unavailable on this tier";
    if (offer.access === "pending") return "Pending approval";
    if (offer.access !== "granted") {
      return canRequest ? (
        <button type="button" className="bc-btn bc-btn-secondary" disabled={busy} onClick={() => {
          void perform(async () => {
            await subscribeModuleOffer(offer.offer_id);
            setMessage("Access request submitted.");
            await refresh();
          });
        }}>Request access</button>
      ) : "Approval required";
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
              <thead><tr><th>Offer</th><th>Module release</th><th>Scope &amp; Rate</th><th>Access</th></tr></thead>
              <tbody>
                {offers.map((offer) => (
                  <tr key={offer.offer_id} data-testid={`module-offer-${offer.offer_id}`}>
                    <th scope="row">
                      {offer.title}
                      <div className="bc-caption">{offer.action === "run" ? "Hosted run" : "Download"}</div>
                    </th>
                    <td className="mono">{offer.plugin_id} · {offer.version}</td>
                    <td>
                      <div>{scopeLabels[offer.scope]}</div>
                      {offer.negotiated_price != null && (
                        <div className="bc-caption mono" data-testid={`module-offer-price-${offer.offer_id}`}>
                          Negotiated ({offer.negotiated_price.manifest_offer_id}): {formatMilliAct(offer.negotiated_price.price_milli_act)}
                          {offer.negotiated_price.base_fee_milli_act ? ` (+${formatMilliAct(offer.negotiated_price.base_fee_milli_act)} base)` : ""}
                        </div>
                      )}
                      {canApprove && offer.scope === "custom_shared" && offer.publisher_org_id === principal.org_id && (
                        editingPriceOfferId === offer.offer_id ? (
                          <div style={{ display: "flex", gap: 4, flexWrap: "wrap", marginTop: 6 }}>
                            <input
                              type="text"
                              className="bc-input mono"
                              style={{ width: 88 }}
                              placeholder="standard"
                              value={editManifestOfferId}
                              onChange={(e) => setEditManifestOfferId(e.target.value)}
                            />
                            <input
                              type="number"
                              step="0.01"
                              min="0"
                              className="bc-input mono"
                              style={{ width: 88 }}
                              placeholder="EUR"
                              value={editPriceEur}
                              onChange={(e) => setEditPriceEur(e.target.value)}
                            />
                            <button
                              type="button"
                              className="bc-btn bc-btn-primary bc-btn-sm"
                              disabled={busy}
                              onClick={() => void saveNegotiatedPrice(offer.offer_id)}
                            >
                              Save EUR
                            </button>
                            <button
                              type="button"
                              className="bc-btn bc-btn-ghost bc-btn-sm"
                              onClick={() => setEditingPriceOfferId(null)}
                            >
                              Cancel
                            </button>
                          </div>
                        ) : (
                          <button
                            type="button"
                            className="bc-btn bc-btn-ghost bc-btn-sm"
                            style={{ marginTop: 4 }}
                            onClick={() => {
                              setEditingPriceOfferId(offer.offer_id);
                              setEditManifestOfferId(offer.negotiated_price?.manifest_offer_id ?? "standard");
                              setEditPriceEur(
                                offer.negotiated_price != null ? (offer.negotiated_price.price_milli_act / 1000).toFixed(2) : "",
                              );
                              setEditBaseFeeEur(
                                offer.negotiated_price != null ? (offer.negotiated_price.base_fee_milli_act / 1000).toFixed(2) : "0",
                              );
                            }}
                          >
                            Set Negotiated EUR
                          </button>
                        )
                      )}
                    </td>
                    <td>{offerControls(offer)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      {(federated.length > 0 || unavailable.length > 0) && (
        <section className="bc-card" aria-label="Partner module offers">
          <div className="bc-card-header"><h2 className="bc-card-title">Partner module offers</h2></div>
          {unavailable.length > 0 && <p className="bc-caption">Unavailable providers: {unavailable.join(", ")}</p>}
          {federated.length > 0 && (
            <div className="table-scroll">
              <table className="data-table">
                <thead><tr><th>Federated offer</th><th>Module / Dataset</th><th>Provider</th><th>Access</th></tr></thead>
                <tbody>
                  {federated.map(({ offer, offer_kind, provider, launch_url }) => {
                    const safeLaunchUrl = launch_url ? sanitizeFederatedLaunchUrl(launch_url) : null;
                    const kind = offer_kind ?? offer.action;
                    const kindLabel =
                      kind === "run"
                        ? "Hosted run"
                        : kind === "download"
                          ? "Download"
                          : kind === "dataset"
                            ? "Dataset"
                            : kind;
                    return (
                      <tr key={`${provider}:${offer.offer_id}:${launch_url ?? ""}`}>
                        <th scope="row">
                          {offer.title}
                          {kindLabel && <div className="bc-caption">{kindLabel}</div>}
                        </th>
                        <td className="mono">{formatFederatedOfferTarget(offer)}</td>
                        <td>{provider}</td>
                        <td>
                          {launch_url ? (
                            safeLaunchUrl ? (
                              <a
                                className="bc-btn bc-btn-secondary"
                                href={safeLaunchUrl}
                                rel="noopener noreferrer"
                              >
                                Open on {provider}
                              </a>
                            ) : (
                              <span className="bc-caption">Blocked unsafe launch URL</span>
                            )
                          ) : (
                            <span className="bc-caption">Synced via federation</span>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}

      {requests.length > 0 && (
        <section className="bc-card" aria-label="Module access requests">
          <div className="bc-card-header"><h2 className="bc-card-title">Module access requests</h2></div>
          <div className="table-scroll">
            <table className="data-table">
              <thead><tr><th>Offer</th><th>Requesting organization</th><th>Status</th>{canApprove && <th>Decision</th>}</tr></thead>
              <tbody>
                {requests.map((request) => (
                  <tr key={`${request.offer_id}:${request.subscriber_org_id}`}>
                    <th scope="row">{request.title}</th>
                    <td className="mono">{request.subscriber_org_id}</td>
                    <td>{request.access === "granted" ? "Approved" : "Pending"}</td>
                    {canApprove && <td>
                      {request.publisher_org_id === principal.org_id && request.access === "pending" && (
                        <button type="button" className="bc-btn bc-btn-primary" disabled={busy}
                          aria-label={`Approve ${request.title} for ${request.subscriber_org_id}`}
                          onClick={() => void perform(async () => {
                            await approveModuleOffer(request.offer_id, request.subscriber_org_id);
                            setMessage("Module offer approved.");
                            await refresh();
                          })}>Approve</button>
                      )}
                    </td>}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>
      )}

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
              <>
                <label className="bc-field">
                  <span className="bc-field-label">Recipient organization IDs</span>
                  <input className="bc-input" required value={recipients} placeholder="org-a, org-b"
                    onChange={(event) => setRecipients(event.target.value)} />
                </label>
                <label className="bc-field">
                  <span className="bc-field-label">Negotiated Offer Tier ID (optional)</span>
                  <input className="bc-input mono" value={manifestOfferId} placeholder="standard"
                    onChange={(event) => setManifestOfferId(event.target.value)} />
                </label>
                <label className="bc-field">
                  <span className="bc-field-label">Negotiated Run Price (EUR, optional)</span>
                  <input type="number" step="0.01" min="0" className="bc-input mono" value={negotiatedPriceEur}
                    placeholder="e.g. 2.50" onChange={(event) => setNegotiatedPriceEur(event.target.value)} />
                </label>
                <label className="bc-field">
                  <span className="bc-field-label">Negotiated Base Fee (EUR, optional)</span>
                  <input type="number" step="0.01" min="0" className="bc-input mono" value={negotiatedBaseFeeEur}
                    placeholder="0.00" onChange={(event) => setNegotiatedBaseFeeEur(event.target.value)} />
                </label>
              </>
            )}
            {scope !== "private" && (
              <label className="bc-field">
                <span className="bc-field-label">Access process</span>
                <select className="bc-input" value={requiresApproval ? "approval" : "self_service"}
                  onChange={(event) => setRequiresApproval(event.target.value === "approval")}>
                  <option value="self_service">Self-service</option>
                  <option value="approval">Publisher approval</option>
                </select>
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
