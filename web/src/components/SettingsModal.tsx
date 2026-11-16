import React, { useEffect, useState } from "react";
import {
  IconCheck,
  IconClose,
  IconDatabase,
  IconModules,
  IconMoon,
  IconSettings,
  IconShield,
  IconSun,
} from "./Icons";
import {
  formatMilliAct,
  type DatasetGrant,
  type HealthStatus,
  type LedgerState,
  type ScaffoldResult,
} from "../types";

export type SettingsSection = "general" | "governance" | "compute" | "developer";

export function SettingsModal({
  open,
  initialSection = "general",
  darkTheme,
  onSetDarkTheme,
  health,
  grants,
  activeGrantId,
  onSelectGrant,
  ledger,
  onScaffoldModule,
  scaffoldOutput,
  onClose,
}: {
  open: boolean;
  initialSection?: SettingsSection;
  darkTheme: boolean;
  onSetDarkTheme: (dark: boolean) => void;
  health: HealthStatus | null;
  grants: DatasetGrant[];
  activeGrantId: string;
  onSelectGrant: (grantId: string) => void;
  ledger: LedgerState | null;
  onScaffoldModule: (payload: {
    template: string;
    plugin_id: string;
    display_name: string;
    include_vite_ui: boolean;
  }) => Promise<void>;
  scaffoldOutput: ScaffoldResult | null;
  onClose: () => void;
}) {
  const [section, setSection] = useState<SettingsSection>(initialSection);
  const [studioTemplate, setStudioTemplate] = useState<string>("python-uv");
  const [studioPluginId, setStudioPluginId] = useState<string>(
    "alithea/custom-neoantigen-scorer",
  );
  const [studioName, setStudioName] = useState<string>("Custom Neoantigen Scorer");
  const [studioIncludeUi, setStudioIncludeUi] = useState<boolean>(true);
  const [scaffolding, setScaffolding] = useState<boolean>(false);

  useEffect(() => {
    if (open) {
      setSection(initialSection);
    }
  }, [open, initialSection]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const handleSubmitScaffold = async (e: React.FormEvent) => {
    e.preventDefault();
    setScaffolding(true);
    try {
      await onScaffoldModule({
        template: studioTemplate,
        plugin_id: studioPluginId,
        display_name: studioName,
        include_vite_ui: studioIncludeUi,
      });
    } finally {
      setScaffolding(false);
    }
  };

  return (
    <div
      className="bc-modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="bc-settings-modal"
        role="dialog"
        aria-modal="true"
        aria-label="Workspace Settings"
        data-testid="settings-modal"
      >
        {/* Left Navigation Column */}
        <aside className="bc-settings-nav">
          <div className="bc-settings-heading">Settings</div>
          <button
            type="button"
            className="bc-settings-tab"
            data-active={String(section === "general")}
            onClick={() => setSection("general")}
          >
            <IconSettings size={15} />
            <span>Appearance</span>
          </button>
          <button
            type="button"
            className="bc-settings-tab"
            data-active={String(section === "governance")}
            onClick={() => setSection("governance")}
          >
            <IconDatabase size={15} />
            <span>Data Governance</span>
          </button>
          <button
            type="button"
            className="bc-settings-tab"
            data-active={String(section === "compute")}
            onClick={() => setSection("compute")}
          >
            <IconShield size={15} />
            <span>Compute &amp; Credits</span>
          </button>
          <button
            type="button"
            className="bc-settings-tab"
            data-active={String(section === "developer")}
            onClick={() => setSection("developer")}
          >
            <IconModules size={15} />
            <span>Developer Studio</span>
          </button>
        </aside>

        {/* Right Content Column */}
        <div className="bc-settings-content">
          <div className="bc-settings-topbar">
            <h2 className="bc-settings-title">
              {section === "general" && "Appearance & Workspace"}
              {section === "governance" && "Data Access & Governance"}
              {section === "compute" && "Compute Node & ACT Credits"}
              {section === "developer" && "Developer Module Studio"}
            </h2>
            <button
              type="button"
              className="bc-win-ctrl"
              aria-label="Close settings"
              onClick={onClose}
            >
              <IconClose size={14} />
            </button>
          </div>

          <div className="bc-settings-scroll">
            {section === "general" && (
              <div className="bc-settings-stack">
                <div>
                  <div className="bc-field-label" style={{ marginBottom: 8 }}>
                    Color Theme
                  </div>
                  <div className="bc-theme-cards">
                    <button
                      type="button"
                      className="bc-theme-card"
                      data-selected={String(!darkTheme)}
                      onClick={() => onSetDarkTheme(false)}
                    >
                      <IconSun size={18} />
                      <div>
                        <div className="bc-theme-card-title">Light (Warm Paper)</div>
                        <div className="bc-caption">
                          Calm daylight palette matched to HLA-Compass
                        </div>
                      </div>
                    </button>
                    <button
                      type="button"
                      className="bc-theme-card"
                      data-selected={String(darkTheme)}
                      onClick={() => onSetDarkTheme(true)}
                    >
                      <IconMoon size={18} />
                      <div>
                        <div className="bc-theme-card-title">Dark (Obsidian)</div>
                        <div className="bc-caption">
                          Low-glare dark surfaces for extended sessions
                        </div>
                      </div>
                    </button>
                  </div>
                </div>

                <div className="bc-card">
                  <div className="bc-card-title">Runtime Architecture</div>
                  <p className="bc-caption" style={{ marginTop: 6, lineHeight: 1.55 }}>
                    BioCompass runs as a single standalone Rust binary serving embedded
                    static assets, DuckDB Parquet governance, hardened rootless OCI
                    containers, and <code>froglet/v1</code> cryptographic receipts — with
                    zero external Node.js or Python server dependencies.
                  </p>
                </div>
              </div>
            )}

            {section === "governance" && (
              <div className="bc-settings-stack">
                <p className="bc-caption">
                  Select the active <code>DatasetGrant</code> applied to DuckDB queries,
                  staged container Parquet slices, and sandboxed module UIs.
                </p>
                <div className="bc-grant-list">
                  {grants.map((g) => {
                    const active = g.grant_id === activeGrantId;
                    return (
                      <button
                        key={g.grant_id}
                        type="button"
                        className="bc-grant-card"
                        data-selected={String(active)}
                        onClick={() => onSelectGrant(g.grant_id)}
                      >
                        <div className="bc-grant-card-top">
                          <strong>{g.label}</strong>
                          {active && (
                            <span className="bc-badge bc-badge-success">
                              <IconCheck size={12} /> Active
                            </span>
                          )}
                        </div>
                        <div className="bc-caption" style={{ marginTop: 4 }}>
                          Organs:{" "}
                          <strong>
                            {g.allowed_organs ? g.allowed_organs.join(", ") : "All tissues"}
                          </strong>{" "}
                          · Conditions:{" "}
                          <strong>
                            {g.allowed_conditions
                              ? g.allowed_conditions.join(", ")
                              : "All conditions"}
                          </strong>
                        </div>
                        <div style={{ marginTop: 8 }}>
                          <span
                            className={`bc-badge ${
                              g.mask_peptide_sequences
                                ? "bc-badge-warn"
                                : "bc-badge-neutral"
                            }`}
                          >
                            {g.mask_peptide_sequences
                              ? "Peptide sequences masked ('***')"
                              : "Unmasked amino-acid sequences"}
                          </span>
                        </div>
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            {section === "compute" && (
              <div className="bc-settings-stack">
                <div className="bc-meta-grid">
                  <div className="bc-meta-item">
                    <span className="bc-caption">Available Balance</span>
                    <strong style={{ fontSize: 16 }}>
                      {ledger?.available_display ?? "0.00 ACT"}
                    </strong>
                  </div>
                  <div className="bc-meta-item">
                    <span className="bc-caption">Container Engine</span>
                    <span className="mono">
                      {health?.container_engine ?? "detecting..."} ({health?.mode ?? "local"})
                    </span>
                  </div>
                  <div className="bc-meta-item">
                    <span className="bc-caption">Froglet Node ID</span>
                    <span className="mono" style={{ wordBreak: "break-all" }}>
                      {health?.node_id ?? "—"}
                    </span>
                  </div>
                  <div className="bc-meta-item">
                    <span className="bc-caption">Agent Model</span>
                    <span className="mono">
                      {health?.llm_configured
                        ? health.llm_model
                        : "Deterministic local skill planner"}
                    </span>
                  </div>
                </div>

                {ledger && ledger.transactions.length > 0 && (
                  <div className="bc-card">
                    <div className="bc-card-title" style={{ marginBottom: 8 }}>
                      ACT Credit Ledger
                    </div>
                    <div className="table-scroll">
                      <table className="data-table">
                        <thead>
                          <tr>
                            <th>Kind</th>
                            <th>Reference</th>
                            <th>Delta</th>
                            <th>Balance</th>
                          </tr>
                        </thead>
                        <tbody>
                          {ledger.transactions.map((tx) => (
                            <tr key={tx.tx_id}>
                              <td className="mono">{tx.kind}</td>
                              <td className="mono">{tx.reference}</td>
                              <td className="mono">{formatMilliAct(tx.delta_act)}</td>
                              <td className="mono">
                                {formatMilliAct(tx.balance_after_act)}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  </div>
                )}
              </div>
            )}

            {section === "developer" && (
              <div className="bc-settings-stack">
                <p className="bc-caption">
                  Generate a new OCI module directory (<code>plugin.toml</code>,{" "}
                  <code>Dockerfile</code>, <code>SKILL.md</code>, and optional sandboxed
                  Vite UI).
                </p>
                <form
                  onSubmit={(e) => void handleSubmitScaffold(e)}
                  className="bc-form-grid"
                >
                  <label className="bc-field">
                    <span className="bc-field-label">Runtime Template</span>
                    <select
                      className="bc-input"
                      value={studioTemplate}
                      onChange={(e) => setStudioTemplate(e.target.value)}
                    >
                      <option value="python-uv">Python 3.12 + uv + Polars</option>
                      <option value="r-rocker">R 4.4 Rocker + Arrow</option>
                      <option value="rust-cli">Rust 2024 Static Binary</option>
                    </select>
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Module ID (namespace/slug)</span>
                    <input
                      type="text"
                      data-testid="studio-plugin-id-input"
                      className="bc-input"
                      value={studioPluginId}
                      onChange={(e) => setStudioPluginId(e.target.value)}
                    />
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Display Name</span>
                    <input
                      type="text"
                      className="bc-input"
                      value={studioName}
                      onChange={(e) => setStudioName(e.target.value)}
                    />
                  </label>

                  <label className="bc-field">
                    <span className="bc-field-label">Custom Interactive UI</span>
                    <select
                      className="bc-input"
                      value={studioIncludeUi ? "yes" : "no"}
                      onChange={(e) => setStudioIncludeUi(e.target.value === "yes")}
                    >
                      <option value="yes">Include Vite sandboxed UI</option>
                      <option value="no">Headless container + declarative charts</option>
                    </select>
                  </label>

                  <div style={{ gridColumn: "1 / -1", marginTop: 4 }}>
                    <button
                      type="submit"
                      data-testid="studio-scaffold-btn"
                      className="bc-btn bc-btn-primary"
                      disabled={scaffolding}
                    >
                      {scaffolding ? "Scaffolding..." : "Create Module Skeleton"}
                    </button>
                  </div>
                </form>

                {scaffoldOutput && (
                  <div className="bc-card" data-testid="scaffold-output">
                    <div className="bc-card-title">
                      Created <span className="mono">{scaffoldOutput.directory}</span>
                    </div>
                    <ul className="bc-caption mono" style={{ marginTop: 8, paddingLeft: 18 }}>
                      {scaffoldOutput.files.map((f) => (
                        <li key={f.relative_path}>{f.relative_path}</li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
