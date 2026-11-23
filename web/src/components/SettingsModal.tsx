import { AppDistributionSettings } from "./AppDistributionSettings";
import { MarketplaceSyncCard } from "./MarketplaceSyncCard";
import { PersonalAccountSettings } from "./PersonalAccountSettings";
import React, { useEffect, useState } from "react";
import { WorkspaceHubChooser } from "./WorkspaceHubChooser";
import { WorkspaceSettings } from "./WorkspaceSettings";
import { isLocalPlayLedgerContext } from "../moduleListPolicy";
import {
  IconCheck,
  IconClose,
  IconDatabase,
  IconEffort,
  IconMode,
  IconModules,
  IconMoon,
  IconSettings,
  IconShield,
  IconSparkles,
  IconSun,
  IconWindowOpen,
  ProviderLogo,
} from "./Icons";
import {
  type PersonalProfile,
  fetchInstalledIdes,
  fetchLlmConfig,
  fetchOrgPluginVersions,
  formatErrorInline,
  testLlmConnection,
  updateLlmConfig,
} from "../api";
import {
  formatWorkspaceAmount,
  type DatasetGrant,
  type DiscoveredLlmBackend,
  type HealthStatus,
  type InstalledIde,
  type LedgerState,
  type LlmConfigResponse,
  type LlmWireProtocol,
  type MarketplaceStatus,
  type PluginVersionRecord,
  type ProviderControlSpec,
  type ScaffoldResult,
  type TestLlmConnectionResponse,
  type UpdateLlmConfigRequest,
  type WorkspaceHubStatus,
} from "../types";

const PREFERRED_IDE_STORAGE_KEY = "biocompass.preferred_ide";
const CUSTOM_IDE_STORAGE_KEY = "biocompass.custom_ide";
const CUSTOM_IDE_OPTION = "__custom__";

export type SettingsSection =
  | "account"
  | "general"
  | "workspace"
  | "agent"
  | "governance"
  | "usage"
  | "developer";

export function SettingsModal({
  open,
  initialSection = "general",
  onProfileUpdated,
  darkTheme,
  onSetDarkTheme,
  health,
  workspaceBearer,
  onWorkspaceBearer,
  initialLlmConfig,
  grants,
  activeGrantId,
  onSelectGrant,
  ledger,
  onScaffoldModule,
  scaffoldOutput,
  onOpenPluginInIde,
  onLlmConfigChanged,
  marketplaceStatus,
  onMarketplaceStatusChanged,
  onWorkspaceHubChanged,
  onClose,
}: {
  open: boolean;
  initialSection?: SettingsSection;
  onProfileUpdated?: (profile: PersonalProfile) => void;
  darkTheme: boolean;
  onSetDarkTheme: (dark: boolean) => void;
  health: HealthStatus | null;
  workspaceBearer?: string;
  onWorkspaceBearer: (value?: string) => void;
  initialLlmConfig?: LlmConfigResponse | null;
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
  onOpenPluginInIde?: (
    directory: string,
    options?: { ide?: string; custom_command?: string },
  ) => Promise<string>;
  onLlmConfigChanged?: (updated?: LlmConfigResponse) => Promise<void> | void;
  marketplaceStatus?: MarketplaceStatus | null;
  onMarketplaceStatusChanged?: (updated: MarketplaceStatus) => void;
  onWorkspaceHubChanged?: (updated: WorkspaceHubStatus) => Promise<void> | void;
  onClose: () => void;
}) {
  const localPlayLedger = isLocalPlayLedgerContext(health, ledger);
  const zeroDisplay = formatWorkspaceAmount(0, localPlayLedger);
  const [section, setSection] = useState<SettingsSection>(initialSection);
  const [studioTemplate, setStudioTemplate] = useState<string>("python-uv");
  const [studioPluginId, setStudioPluginId] = useState<string>(
    "alithea/custom-neoantigen-scorer",
  );
  const [studioName, setStudioName] = useState<string>("Custom Neoantigen Scorer");
  const [studioIncludeUi, setStudioIncludeUi] = useState<boolean>(true);
  const [scaffolding, setScaffolding] = useState<boolean>(false);
  const [installedIdes, setInstalledIdes] = useState<InstalledIde[]>([]);
  const [defaultIdeId, setDefaultIdeId] = useState<string | null>(null);
  const [selectedIde, setSelectedIde] = useState<string>(() => {
    try {
      return window.localStorage.getItem(PREFERRED_IDE_STORAGE_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [customIdeCommand, setCustomIdeCommand] = useState<string>(() => {
    try {
      return window.localStorage.getItem(CUSTOM_IDE_STORAGE_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [openingIde, setOpeningIde] = useState<boolean>(false);
  const [launchedIde, setLaunchedIde] = useState<string | null>(null);
  const [ideError, setIdeError] = useState<string | null>(null);
  const [orgPublishedPlugins, setOrgPublishedPlugins] = useState<PluginVersionRecord[]>([]);

  const [llmConfig, setLlmConfig] = useState<LlmConfigResponse | null>(initialLlmConfig ?? null);
  const [loadingLlm, setLoadingLlm] = useState<boolean>(false);
  const [selectedBackendId, setSelectedBackendId] = useState<string>("codex");
  const [draftProtocol, setDraftProtocol] = useState<LlmWireProtocol>("codex_cli");
  const [draftCommand, setDraftCommand] = useState<string>("codex");
  const [draftArgsText, setDraftArgsText] = useState<string>("");
  const [draftBaseUrl, setDraftBaseUrl] = useState<string>("https://api.openai.com");
  const [draftApiKey, setDraftApiKey] = useState<string>("");
  const [draftModel, setDraftModel] = useState<string>("");
  const [draftConfigOptions, setDraftConfigOptions] = useState<Record<string, string>>({});
  const [savingLlm, setSavingLlm] = useState<boolean>(false);
  const [testingLlm, setTestingLlm] = useState<boolean>(false);
  const [llmError, setLlmError] = useState<string | null>(null);
  const [llmStatusMsg, setLlmStatusMsg] = useState<string | null>(null);
  const [llmTestResult, setLlmTestResult] = useState<TestLlmConnectionResponse | null>(
    null,
  );

  useEffect(() => {
    if (open) {
      setSection(initialSection);
    }
  }, [open, initialSection]);

  const hydrateDraftFromResponse = (res: LlmConfigResponse) => {
    setLlmConfig(res);
    if (res.active) {
      setDraftProtocol(res.active.protocol);
      setDraftCommand(res.active.command);
      setDraftArgsText(res.active.args.join(" "));
      setDraftBaseUrl(res.active.base_url || "https://api.openai.com");
      setDraftApiKey("");
      setDraftModel(res.active.model);
      setDraftConfigOptions(res.active.config_options ?? {});
      if (res.active.protocol === "codex_cli") {
        setSelectedBackendId("codex");
      } else if (res.active.protocol === "claude_code_cli") {
        setSelectedBackendId("claude_code");
      } else if (res.active.protocol === "acp_stdio") {
        const cmd = res.active.command.toLowerCase();
        if (cmd.includes("vibe")) setSelectedBackendId("vibe_acp");
        else if (cmd.includes("opencode")) setSelectedBackendId("opencode_acp");
        else if (cmd.includes("gemini")) setSelectedBackendId("gemini_acp");
        else setSelectedBackendId("custom_acp");
      } else if (res.active.base_url.includes("anthropic")) {
        setSelectedBackendId("anthropic");
      } else if (res.active.base_url.includes("mistral")) {
        setSelectedBackendId("mistral");
      } else if (res.active.base_url.includes("deepseek")) {
        setSelectedBackendId("deepseek");
      } else if (
        res.active.base_url.includes("127.0.0.1") ||
        res.active.base_url.includes("localhost")
      ) {
        setSelectedBackendId("ollama");
      } else {
        setSelectedBackendId("openai");
      }
    } else if (res.local_management_enabled && res.discovered_backends.length > 0) {
      const signedIn =
        res.discovered_backends.find((b) => b.auth_state === "signed_in") ??
        res.discovered_backends.find((b) => b.installed) ??
        res.discovered_backends[0];
      if (signedIn) {
        setSelectedBackendId(signedIn.id);
        setDraftProtocol(signedIn.protocol);
        setDraftCommand(signedIn.resolved_path ?? signedIn.command);
        setDraftArgsText(signedIn.default_args.join(" "));
        setDraftModel(signedIn.suggested_models.find((m) => m.length > 0) ?? "");
        setDraftConfigOptions({});
      }
    }
  };

  useEffect(() => {
    if (open && initialLlmConfig) {
      hydrateDraftFromResponse(initialLlmConfig);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const refreshLlmState = async () => {
    setLoadingLlm(true);
    setLlmError(null);
    try {
      const res = await fetchLlmConfig();
      hydrateDraftFromResponse(res);
    } catch (err: unknown) {
      setLlmError(formatErrorInline(err));
    } finally {
      setLoadingLlm(false);
    }
  };

  useEffect(() => {
    if (!open) return;
    void refreshLlmState();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (!open || (health?.mode !== "cloud" && health?.mode !== "cloud-web")) return;
    let cancelled = false;
    void fetchOrgPluginVersions()
      .then((list) => {
        if (!cancelled) setOrgPublishedPlugins(list);
      })
      .catch(() => {
        // Viewer/member roles may not have developer list permission.
      });
    return () => {
      cancelled = true;
    };
  }, [open, health?.mode]);

  useEffect(() => {
    if (!open || health?.mode !== "local") return;
    let cancelled = false;
    void fetchInstalledIdes()
      .then((res) => {
        if (cancelled) return;
        setInstalledIdes(res.ides);
        setDefaultIdeId(res.default_ide_id);
        setSelectedIde((prev) => {
          if (prev === CUSTOM_IDE_OPTION) return prev;
          if (prev && res.ides.some((item) => item.id === prev)) return prev;
          return res.default_ide_id ?? res.ides[0]?.id ?? CUSTOM_IDE_OPTION;
        });
      })
      .catch(() => {
        // Keep selector usable with custom command if discovery fails.
      });
    return () => {
      cancelled = true;
    };
  }, [open, health?.mode]);

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
    setLaunchedIde(null);
    setIdeError(null);
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

  const handleSelectIde = (value: string) => {
    setSelectedIde(value);
    setLaunchedIde(null);
    setIdeError(null);
    try {
      window.localStorage.setItem(PREFERRED_IDE_STORAGE_KEY, value);
    } catch {
      // Ignore storage quota / privacy mode errors.
    }
  };

  const handleChangeCustomIde = (value: string) => {
    setCustomIdeCommand(value);
    setLaunchedIde(null);
    setIdeError(null);
    try {
      window.localStorage.setItem(CUSTOM_IDE_STORAGE_KEY, value);
    } catch {
      // Ignore storage quota / privacy mode errors.
    }
  };

  const handleOpenInIde = async (directory: string) => {
    if (!onOpenPluginInIde) return;
    if (selectedIde === CUSTOM_IDE_OPTION && !customIdeCommand.trim()) {
      setIdeError("Enter an application name or IDE CLI command");
      return;
    }
    setOpeningIde(true);
    setLaunchedIde(null);
    setIdeError(null);
    try {
      const options =
        selectedIde === CUSTOM_IDE_OPTION
          ? { custom_command: customIdeCommand.trim() }
          : selectedIde
            ? { ide: selectedIde }
            : undefined;
      const launched = await onOpenPluginInIde(directory, options);
      setLaunchedIde(launched);
    } catch (err: unknown) {
      setIdeError(err instanceof Error ? err.message : String(err));
    } finally {
      setOpeningIde(false);
    }
  };

  const isLocalCliProtocol = (p: LlmWireProtocol) =>
    p === "acp_stdio" || p === "codex_cli" || p === "claude_code_cli";

  const buildUpdatePayload = (): UpdateLlmConfigRequest => {
    const args = draftArgsText
      .split(/\s+/)
      .map((s) => s.trim())
      .filter(Boolean);
    if (isLocalCliProtocol(draftProtocol)) {
      return {
        protocol: draftProtocol,
        command: draftCommand.trim(),
        args,
        model: draftModel.trim(),
        config_options: draftConfigOptions,
      };
    }
    return {
      protocol: draftProtocol,
      base_url: draftBaseUrl.trim(),
      ...(draftApiKey.trim() ? { api_key: draftApiKey.trim() } : {}),
      model: draftModel.trim(),
      config_options: draftConfigOptions,
    };
  };

  const handleSelectDiscoveredBackend = (b: DiscoveredLlmBackend) => {
    setSelectedBackendId(b.id);
    setDraftProtocol(b.protocol);
    setDraftCommand(b.resolved_path ?? b.command);
    setDraftArgsText(b.default_args.join(" "));
    setDraftModel(b.suggested_models.find((m) => m.length > 0) ?? "");
    setDraftConfigOptions({});
    setLlmError(null);
    setLlmStatusMsg(null);
    setLlmTestResult(null);
  };

  const handleSelectCustomAcp = () => {
    setSelectedBackendId("custom_acp");
    setDraftProtocol("acp_stdio");
    if (!draftCommand || draftCommand === "codex" || draftCommand === "claude") {
      setDraftCommand("opencode");
      setDraftArgsText("acp");
    }
    setDraftModel("");
    setDraftConfigOptions({});
    setLlmError(null);
    setLlmStatusMsg(null);
    setLlmTestResult(null);
  };

  const handleSelectHttpPreset = (
    presetId: "openai" | "anthropic" | "mistral" | "deepseek" | "ollama",
  ) => {
    setSelectedBackendId(presetId);
    if (presetId === "anthropic") {
      setDraftProtocol("anthropic_messages");
      setDraftBaseUrl("https://api.anthropic.com");
      setDraftModel("claude-sonnet-4-20250514");
    } else if (presetId === "mistral") {
      setDraftProtocol("open_ai_chat_completions");
      setDraftBaseUrl("https://api.mistral.ai");
      setDraftModel("mistral-large-latest");
    } else if (presetId === "deepseek") {
      setDraftProtocol("open_ai_chat_completions");
      setDraftBaseUrl("https://api.deepseek.com");
      setDraftModel("deepseek-chat");
    } else if (presetId === "ollama") {
      setDraftProtocol("open_ai_chat_completions");
      setDraftBaseUrl("http://127.0.0.1:11434");
      setDraftModel("qwen2.5:14b");
    } else {
      setSelectedBackendId("openai");
      setDraftProtocol("open_ai_chat_completions");
      setDraftBaseUrl("https://api.openai.com");
      setDraftModel("gpt-4o");
    }
    setDraftConfigOptions({});
    setLlmError(null);
    setLlmStatusMsg(null);
    setLlmTestResult(null);
  };

  const handleSaveLlm = async (e?: React.FormEvent) => {
    if (e) e.preventDefault();
    setSavingLlm(true);
    setLlmError(null);
    setLlmStatusMsg(null);
    setLlmTestResult(null);
    try {
      const updated = await updateLlmConfig(buildUpdatePayload());
      hydrateDraftFromResponse(updated);
      setLlmStatusMsg(`Connected to ${updated.display_name ?? "selected backend"}`);
      await onLlmConfigChanged?.(updated);
    } catch (err: unknown) {
      setLlmError(formatErrorInline(err));
    } finally {
      setSavingLlm(false);
    }
  };

  const handleTestLlm = async () => {
    setTestingLlm(true);
    setLlmError(null);
    setLlmStatusMsg(null);
    setLlmTestResult(null);
    try {
      const res = await testLlmConnection(buildUpdatePayload());
      setLlmTestResult(res);
    } catch (err: unknown) {
      setLlmError(formatErrorInline(err));
    } finally {
      setTestingLlm(false);
    }
  };

  const handleDisconnectLlm = async () => {
    setSavingLlm(true);
    setLlmError(null);
    setLlmStatusMsg(null);
    setLlmTestResult(null);
    try {
      const updated = await updateLlmConfig({ disconnect: true });
      setLlmConfig(updated);
      setLlmStatusMsg("Disconnected agent backend.");
      await onLlmConfigChanged?.(updated);
    } catch (err: unknown) {
      setLlmError(formatErrorInline(err));
    } finally {
      setSavingLlm(false);
    }
  };

  const matchesActiveDraft = Boolean(
    llmConfig?.active &&
      llmConfig.active.protocol === draftProtocol &&
      (isLocalCliProtocol(draftProtocol)
        ? llmConfig.active.command === draftCommand.trim() &&
          llmConfig.active.args.join(" ") === draftArgsText.trim()
        : llmConfig.active.base_url === draftBaseUrl.trim().replace(/\/+$/, "")),
  );

  const rawDynamicControls: ProviderControlSpec[] =
    llmTestResult?.controls ?? (matchesActiveDraft ? llmConfig?.active_controls ?? [] : []);
  const rawPerModelControls =
    llmTestResult?.per_model_controls ??
    (matchesActiveDraft ? llmConfig?.per_model_controls ?? {} : {});

  const effectiveDynamicControls: ProviderControlSpec[] = (() => {
    const modelKey = draftModel.trim() || rawDynamicControls.find((c) => c.id === "model")?.current_value || "";
    const modelExtra = rawPerModelControls[modelKey] ?? [];
    const merged = rawDynamicControls.map((c) => {
      if (c.id === "model") {
        return { ...c, current_value: draftModel.trim() || c.current_value };
      }
      const override = modelExtra.find((m) => m.id === c.id);
      const base = override ?? c;
      return {
        ...base,
        current_value: draftConfigOptions[base.id] ?? base.current_value,
      };
    });
    for (const extra of modelExtra) {
      if (!merged.some((m) => m.id === extra.id)) {
        merged.push({
          ...extra,
          current_value: draftConfigOptions[extra.id] ?? extra.current_value,
        });
      }
    }
    return merged;
  })();

  const nonModelDynamicControls = effectiveDynamicControls.filter(
    (c) => c.id !== "model" && c.options.length > 0,
  );

  const activeDiscovered = llmConfig?.discovered_backends.find(
    (b) => b.id === selectedBackendId,
  );
  const candidateModels = Array.from(
    new Set([
      ...(effectiveDynamicControls.find((c) => c.id === "model")?.options.map((o) => o.value) ?? []),
      ...(llmTestResult?.available_models ?? []),
      ...(activeDiscovered?.suggested_models.filter((m) => m.length > 0) ?? []),
    ]),
  );

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
          <button type="button" className="bc-settings-tab"
            data-active={String(section === "workspace")} onClick={() => setSection("workspace")}>
            <IconDatabase size={15} />
            <span>Workspace</span>
          </button>
          <button
            type="button"
            className="bc-settings-tab"
            data-active={String(section === "agent")}
            data-testid="settings-tab-agent"
            onClick={() => setSection("agent")}
          >
            <IconSparkles size={15} />
            <span>Agent / LLM</span>
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
            data-active={String(section === "usage")}
            data-testid="settings-tab-usage"
            onClick={() => setSection("usage")}
          >
            <IconShield size={15} />
            <span style={{ flex: 1, textAlign: "left" }}>Usage</span>
            <span className="bc-settings-tab-count mono" data-testid="settings-usage-count">
              {ledger?.available_display ?? zeroDisplay}
            </span>
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
          {(health?.mode === "cloud-web" || health?.mode === "cloud") && (
            <button type="button" className="bc-settings-tab" data-active={String(section === "account")}
              onClick={() => setSection("account")}>Account</button>
          )}
        </aside>

        {/* Right Content Column */}
        <div className="bc-settings-content">
          <div className="bc-settings-topbar">
            <h2 className="bc-settings-title">
              {section === "general" && "Appearance"}
              {section === "workspace" && "Workspace"}
              {section === "agent" && "Agent Backend & Subscription Connection"}
              {section === "governance" && "Data Access & Governance"}
              {section === "usage" && "Usage & Credits"}
              {section === "developer" && "Developer Module Studio"}
              {section === "account" && "Personal account"}
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
            {section === "workspace" && (
              <div className="bc-settings-stack">
                {health?.mode === "local" && (
                  <WorkspaceHubChooser
                    initialStatus={health.workspace_hub ?? null}
                    onHubChanged={onWorkspaceHubChanged}
                  />
                )}
                <WorkspaceSettings
                  hub={health?.workspace_hub_url}
                  bearer={workspaceBearer}
                  onBearer={onWorkspaceBearer}
                />
              </div>
            )}
            {section === "general" && (
              <div className="bc-settings-stack">
                <MarketplaceSyncCard
                  isLocalMode={health?.mode === "local"}
                  status={marketplaceStatus ?? health?.marketplace_status ?? null}
                  onStatusChanged={onMarketplaceStatusChanged}
                />
                <AppDistributionSettings local={health?.mode === "local"} />
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

            {section === "agent" && (
              <div className="bc-settings-stack bc-llm-compact-stack">
                {loadingLlm && !llmConfig ? (
                  <div className="bc-card bc-llm-compact-card">
                    <div className="bc-card-title">Scanning Local Agents &amp; Subscriptions…</div>
                    <p className="bc-caption" style={{ marginTop: 4 }}>
                      Detecting Codex, Claude Code, Mistral Vibe, OpenCode, and Gemini CLIs.
                    </p>
                  </div>
                ) : (
                <>
                <div className="bc-card bc-llm-compact-card">
                  <div className="bc-card-header">
                    <div className="bc-llm-active-summary">
                      <ProviderLogo
                        provider={llmConfig?.active?.provider_id ?? selectedBackendId}
                        size={18}
                      />
                      <div>
                        <div className="bc-card-title">
                          {llmConfig?.configured
                            ? llmConfig.display_name
                            : "No model backend connected"}
                        </div>
                        <div className="bc-caption">
                          {llmConfig?.configured
                            ? `Active (${llmConfig.active?.is_local_cli ? "Local CLI / ACP" : "HTTP API"})`
                            : "Select a subscription CLI or enter an API key below."}
                        </div>
                      </div>
                    </div>
                    <div className="bc-llm-header-actions">
                      <span
                        className={`bc-badge ${
                          llmConfig?.configured ? "bc-badge-success" : "bc-badge-warn"
                        }`}
                      >
                        {llmConfig?.configured ? "Connected" : "Not connected"}
                      </span>
                      {llmConfig?.local_management_enabled && llmConfig?.configured && (
                        <button
                          type="button"
                          className="bc-btn bc-btn-ghost bc-btn-sm"
                          onClick={() => void handleDisconnectLlm()}
                          disabled={savingLlm}
                        >
                          Disconnect
                        </button>
                      )}
                    </div>
                  </div>
                </div>

                {!llmConfig?.local_management_enabled && (
                  <div className="bc-card bc-llm-compact-card">
                    <div className="bc-card-title">Managed Cloud Mode</div>
                    <p className="bc-caption" style={{ marginTop: 4, lineHeight: 1.45 }}>
                      Local CLI / ACP subprocess discovery is disabled in{" "}
                      <code>{health?.mode ?? "cloud-web"}</code> mode. Start locally with{" "}
                      <code>biocompass up</code> to connect local CLIs.
                    </p>
                  </div>
                )}

                {llmConfig?.local_management_enabled && (
                  <>
                    <div>
                      <div className="bc-llm-section-row">
                        <span className="bc-field-label">
                          Subscriptions &amp; ACP Agents
                        </span>
                        <button
                          type="button"
                          className="bc-btn bc-btn-ghost bc-btn-sm"
                          onClick={() => void refreshLlmState()}
                          disabled={loadingLlm}
                        >
                          {loadingLlm ? "Scanning…" : "Rescan"}
                        </button>
                      </div>
                      <div className="bc-llm-preset-grid">
                        {llmConfig.discovered_backends.map((b) => {
                          const selected = selectedBackendId === b.id;
                          const badgeClass =
                            b.auth_state === "signed_in"
                              ? "bc-badge-success"
                              : b.auth_state === "needs_login"
                                ? "bc-badge-warn"
                                : b.auth_state === "ready"
                                  ? "bc-badge-accent"
                                  : "bc-badge-neutral";
                          const badgeText =
                            b.auth_state === "signed_in"
                              ? "Signed in"
                              : b.auth_state === "needs_login"
                                ? "Login needed"
                                : b.auth_state === "ready"
                                  ? "ACP ready"
                                  : "Not installed";
                          return (
                            <button
                              key={b.id}
                              type="button"
                              className="bc-llm-preset-card"
                              data-selected={String(selected)}
                              data-testid={`llm-preset-${b.id}`}
                              title={`${b.label} — ${b.subtitle}`}
                              onClick={() => handleSelectDiscoveredBackend(b)}
                            >
                              <div className="bc-llm-preset-top">
                                <span className="bc-llm-preset-brand">
                                  <ProviderLogo provider={b.id} size={15} />
                                  <strong>{b.label.replace(/\s*\(.*\)$/, "")}</strong>
                                </span>
                                <span className={`bc-badge ${badgeClass}`}>{badgeText}</span>
                              </div>
                              <div className="bc-llm-preset-detail mono">
                                {b.auth_detail}
                              </div>
                            </button>
                          );
                        })}

                        <button
                          type="button"
                          className="bc-llm-preset-card"
                          data-selected={String(selectedBackendId === "custom_acp")}
                          title="Connect any custom ACP stdio CLI"
                          onClick={handleSelectCustomAcp}
                        >
                          <div className="bc-llm-preset-top">
                            <span className="bc-llm-preset-brand">
                              <ProviderLogo provider="acp" size={15} />
                              <strong>Custom ACP</strong>
                            </span>
                            <span className="bc-badge bc-badge-accent">Stdio</span>
                          </div>
                          <div className="bc-llm-preset-detail mono">
                            claude-code-acp, codex-acp, or custom CLI
                          </div>
                        </button>
                      </div>
                    </div>

                    <div className="bc-llm-http-row">
                      <span className="bc-field-label">HTTP API</span>
                      <div className="bc-llm-http-pills">
                        {(
                          [
                            ["openai", "OpenAI"],
                            ["anthropic", "Anthropic"],
                            ["mistral", "Mistral"],
                            ["deepseek", "DeepSeek"],
                            ["ollama", "Ollama"],
                          ] as const
                        ).map(([id, label]) => (
                          <button
                            key={id}
                            type="button"
                            className="bc-btn bc-btn-ghost bc-btn-sm"
                            data-active={String(selectedBackendId === id)}
                            onClick={() => handleSelectHttpPreset(id)}
                          >
                            <ProviderLogo provider={id} size={13} />
                            <span>{label}</span>
                          </button>
                        ))}
                      </div>
                    </div>

                    <form className="bc-card bc-llm-form bc-llm-compact-card" onSubmit={(e) => void handleSaveLlm(e)}>
                      {activeDiscovered &&
                        activeDiscovered.auth_state === "needs_login" &&
                        activeDiscovered.login_command && (
                          <div className="bc-llm-login-hint">
                            <span>Login required:</span>
                            <code>{activeDiscovered.login_command}</code>
                          </div>
                        )}

                      <div className="bc-form-grid bc-llm-form-grid">
                        {isLocalCliProtocol(draftProtocol) ? (
                          <>
                            <label className="bc-field">
                              <span className="bc-field-label">Command</span>
                              <input
                                className="bc-input bc-input-compact mono"
                                value={draftCommand}
                                onChange={(e) => setDraftCommand(e.target.value)}
                                placeholder="codex / claude / opencode / gemini"
                                required
                              />
                            </label>

                            {draftProtocol === "acp_stdio" && (
                              <label className="bc-field">
                                <span className="bc-field-label">ACP Args</span>
                                <input
                                  className="bc-input bc-input-compact mono"
                                  value={draftArgsText}
                                  onChange={(e) => setDraftArgsText(e.target.value)}
                                  placeholder="acp (or --acp)"
                                />
                              </label>
                            )}
                          </>
                        ) : (
                          <>
                            <label className="bc-field">
                              <span className="bc-field-label">Wire Protocol</span>
                              <select
                                className="bc-input bc-input-compact"
                                value={draftProtocol}
                                onChange={(e) =>
                                  setDraftProtocol(e.target.value as LlmWireProtocol)
                                }
                              >
                                <option value="open_ai_chat_completions">
                                  OpenAI (/v1/chat/completions)
                                </option>
                                <option value="anthropic_messages">
                                  Anthropic (/v1/messages)
                                </option>
                              </select>
                            </label>

                            <label className="bc-field">
                              <span className="bc-field-label">Base URL</span>
                              <input
                                className="bc-input bc-input-compact mono"
                                value={draftBaseUrl}
                                onChange={(e) => setDraftBaseUrl(e.target.value)}
                                placeholder="https://api.openai.com"
                                required
                              />
                            </label>

                            <label className="bc-field">
                              <span className="bc-field-label">
                                API Key{" "}
                                {llmConfig.active?.has_api_key &&
                                llmConfig.active.base_url === draftBaseUrl.trim().replace(/\/+$/, "")
                                  ? `(${llmConfig.active.api_key_hint})`
                                  : "(optional locally)"}
                              </span>
                              <input
                                className="bc-input bc-input-compact mono"
                                type="password"
                                autoComplete="off"
                                value={draftApiKey}
                                onChange={(e) => setDraftApiKey(e.target.value)}
                                placeholder={
                                  llmConfig.active?.has_api_key ? "••••••••" : "sk-..."
                                }
                              />
                            </label>
                          </>
                        )}

                        <label className="bc-field">
                          <span className="bc-field-label">
                            Model{isLocalCliProtocol(draftProtocol) ? " (blank = default)" : ""}
                          </span>
                          <input
                            className="bc-input bc-input-compact mono"
                            value={draftModel}
                            onChange={(e) => setDraftModel(e.target.value)}
                            placeholder={
                              isLocalCliProtocol(draftProtocol)
                                ? "default (or choose below)"
                                : "gpt-4o / deepseek-chat"
                            }
                            required={!isLocalCliProtocol(draftProtocol)}
                          />
                        </label>

                        {nonModelDynamicControls.map((ctrl) => (
                          <label key={ctrl.id} className="bc-field">
                            <span className="bc-field-label bc-field-label-icon">
                              {ctrl.id === "mode" ? (
                                <IconMode size={12} />
                              ) : (
                                <IconEffort size={12} />
                              )}
                              <span>{ctrl.label}</span>
                            </span>
                            <select
                              className="bc-input bc-input-compact mono"
                              value={ctrl.current_value}
                              onChange={(e) =>
                                setDraftConfigOptions((prev) => ({
                                  ...prev,
                                  [ctrl.id]: e.target.value,
                                }))
                              }
                            >
                              {ctrl.options.map((opt) => (
                                <option key={opt.value} value={opt.value}>
                                  {opt.label}
                                  {opt.description ? ` — ${opt.description}` : ""}
                                </option>
                              ))}
                            </select>
                          </label>
                        ))}
                      </div>

                      {candidateModels.length > 0 && (
                        <div className="bc-llm-models-block">
                          {candidateModels.length > 6 && (
                            <label className="bc-field" style={{ marginTop: 10 }}>
                              <span className="bc-field-label">
                                Discovered Models ({candidateModels.length})
                              </span>
                              <select
                                className="bc-input mono"
                                value={
                                  candidateModels.includes(draftModel.trim())
                                    ? draftModel.trim()
                                    : ""
                                }
                                onChange={(e) => {
                                  if (e.target.value) setDraftModel(e.target.value);
                                }}
                              >
                                <option value="">
                                  Select from {candidateModels.length} discovered models…
                                </option>
                                {candidateModels.map((m) => (
                                  <option key={m} value={m}>
                                    {m}
                                  </option>
                                ))}
                              </select>
                            </label>
                          )}
                          <div className="bc-llm-model-chips">
                            <span className="bc-caption">Quick select model:</span>
                            {candidateModels.slice(0, 6).map((m) => (
                              <button
                                key={m}
                                type="button"
                                className="bc-llm-model-chip mono"
                                data-selected={String(draftModel.trim() === m)}
                                onClick={() => setDraftModel(m)}
                              >
                                {m}
                              </button>
                            ))}
                          </div>
                        </div>
                      )}

                      <div className="bc-llm-actions">
                        <button
                          type="button"
                          className="bc-btn bc-btn-ghost"
                          onClick={() => void handleTestLlm()}
                          disabled={testingLlm || savingLlm}
                          data-testid="llm-test-btn"
                        >
                          {testingLlm ? "Testing…" : "Test Connection"}
                        </button>
                        <button
                          type="submit"
                          className="bc-btn bc-btn-primary"
                          disabled={savingLlm || testingLlm}
                          data-testid="llm-save-btn"
                        >
                          {savingLlm ? "Connecting…" : "Save & Connect"}
                        </button>
                      </div>

                      {llmTestResult && (
                        <div
                          className={
                            llmTestResult.ok ? "bc-inline-success" : "bc-inline-error"
                          }
                          style={{ marginTop: 10 }}
                        >
                          {llmTestResult.message}
                        </div>
                      )}
                      {llmStatusMsg && (
                        <div
                          className="bc-inline-success"
                          data-testid="llm-save-status"
                          style={{ marginTop: 10 }}
                        >
                          {llmStatusMsg}
                        </div>
                      )}
                      {llmError && (
                        <div className="bc-inline-error" style={{ marginTop: 10 }}>
                          {llmError}
                        </div>
                      )}
                    </form>
                  </>
                )}
                </>
                )}
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
                          Level: <strong className="mono">{g.level}</strong> · Max/Query:{" "}
                          <strong className="mono">
                            {g.max_rows_per_query.toLocaleString()}
                          </strong>
                          {g.daily_row_budget != null && (
                            <>
                              {" "}
                              · Daily Budget:{" "}
                              <strong className="mono">
                                {g.daily_row_budget.toLocaleString()}
                              </strong>
                            </>
                          )}
                        </div>
                        <div style={{ marginTop: 8, display: "flex", gap: 6, flexWrap: "wrap" }}>
                          <span
                            className={`bc-badge ${
                              g.level === "compute_only"
                                ? "bc-badge-warn"
                                : "bc-badge-neutral"
                            }`}
                          >
                            {g.level === "compute_only"
                              ? "Compute-Only (Curated Modules)"
                              : g.row_scope
                                ? "Scoped Unmasked Rows"
                                : "Full Unmasked Row Access"}
                          </span>
                          {g.level === "rows" && (
                            <span className="bc-badge bc-badge-neutral">
                              {g.allow_local_export ? "Local Export Enabled" : "No Local Export"}
                            </span>
                          )}
                        </div>
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            {section === "usage" && (() => {
              const txs = ledger?.transactions ?? [];
              const totalSpentMilli = txs
                .filter((tx) => tx.delta_act < 0)
                .reduce((acc, tx) => acc + Math.abs(tx.delta_act), 0);
              return (
                <div className="bc-settings-stack">
                  <div className="bc-meta-grid">
                    <div className="bc-meta-item">
                      <span className="bc-caption">
                        {localPlayLedger ? "Local Play Credits" : "Available Balance (EUR)"}
                      </span>
                      <strong
                        style={{ fontSize: 18, color: "var(--bc-accent)" }}
                        className="mono"
                        data-testid="usage-available-credits"
                      >
                        {ledger?.available_display ?? zeroDisplay}
                      </strong>
                    </div>
                    <div className="bc-meta-item">
                      <span className="bc-caption">
                        {localPlayLedger ? "Play Credits Used" : "EUR Spent"}
                      </span>
                      <strong style={{ fontSize: 16 }} className="mono">
                        {formatWorkspaceAmount(totalSpentMilli, localPlayLedger)}
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
                  </div>

                  {txs.length > 0 && (
                    <div className="bc-card">
                      <div className="bc-card-header" style={{ marginBottom: 8 }}>
                        <span className="bc-card-title">Credit Usage Ledger</span>
                        <span className="bc-caption">{txs.length} entries</span>
                      </div>
                      <div className="table-scroll">
                        <table className="data-table">
                          <thead>
                            <tr>
                              <th>Kind</th>
                              <th>Reference</th>
                              <th style={{ textAlign: "right" }}>Delta</th>
                              <th style={{ textAlign: "right" }}>Balance</th>
                            </tr>
                          </thead>
                          <tbody>
                            {txs.map((tx) => (
                              <tr key={tx.tx_id}>
                                <td className="mono">{tx.kind}</td>
                                <td
                                  className="mono"
                                  title={tx.reference}
                                  style={{
                                    maxWidth: 150,
                                    overflow: "hidden",
                                    textOverflow: "ellipsis",
                                  }}
                                >
                                  {tx.reference}
                                </td>
                                <td className="mono" style={{ textAlign: "right" }}>
                                  {formatWorkspaceAmount(tx.delta_act, localPlayLedger)}
                                </td>
                                <td className="mono" style={{ textAlign: "right" }}>
                                  {formatWorkspaceAmount(tx.balance_after_act, localPlayLedger)}
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    </div>
                  )}
                </div>
              );
            })()}

            {section === "account" && <PersonalAccountSettings onProfileUpdated={onProfileUpdated} />}
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
                      <option value="r-bioc">R 4.4 Bioconductor + Arrow</option>
                      <option value="nextflow">Nextflow DSL2 Pipeline</option>
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

                {scaffoldOutput && (() => {
                  const createdFiles = Array.isArray(scaffoldOutput.files)
                    ? scaffoldOutput.files.map((f) => f.relative_path)
                    : scaffoldOutput.files && typeof scaffoldOutput.files === "object"
                      ? Object.keys(scaffoldOutput.files as Record<string, string>)
                      : [];
                  return (
                    <div className="bc-card" data-testid="scaffold-output">
                      <div
                        style={{
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "space-between",
                          gap: 8,
                          flexWrap: "wrap",
                        }}
                      >
                        <div className="bc-card-title">
                          Created <span className="mono">{scaffoldOutput.directory}</span>
                        </div>
                        <span
                          className={`bc-badge ${
                            scaffoldOutput.registered ? "bc-badge-success" : "bc-badge-neutral"
                          }`}
                        >
                          {scaffoldOutput.registered
                            ? "Registered in Workspace"
                            : "Saved on Disk"}
                        </span>
                      </div>
                      <ul className="bc-caption mono" style={{ marginTop: 8, paddingLeft: 18 }}>
                        {createdFiles.map((path) => (
                          <li key={path}>{path}</li>
                        ))}
                      </ul>
                      {!scaffoldOutput.registered && (
                        <p className="bc-caption" style={{ marginTop: 8, marginBottom: 0 }}>
                          Build and push the module OCI image, pin its{" "}
                          <code>@sha256:</code> digest in <code>plugin.toml</code> (or launch
                          with <code>--allow-local-dev-tags</code>), then restart to load it
                          into the Modules grid.
                        </p>
                      )}
                      {health?.mode === "local" && onOpenPluginInIde && (
                        <div style={{ marginTop: 12, display: "flex", flexDirection: "column", gap: 8 }}>
                          <div
                            style={{
                              display: "flex",
                              alignItems: "center",
                              gap: 8,
                              flexWrap: "wrap",
                            }}
                          >
                            <select
                              data-testid="ide-select"
                              aria-label="Target IDE or editor"
                              className="bc-input"
                              style={{ width: "auto", minWidth: 210 }}
                              value={selectedIde || CUSTOM_IDE_OPTION}
                              onChange={(e) => handleSelectIde(e.target.value)}
                            >
                              {installedIdes.map((ide) => (
                                <option key={ide.id} value={ide.id}>
                                  {ide.label}
                                  {ide.id === defaultIdeId ? " (OS Default)" : ""}
                                </option>
                              ))}
                              <option value={CUSTOM_IDE_OPTION}>
                                Custom app or CLI command...
                              </option>
                            </select>
                            <button
                              type="button"
                              className="bc-btn bc-btn-secondary"
                              data-testid="open-default-ide-btn"
                              disabled={openingIde}
                              onClick={() => handleOpenInIde(scaffoldOutput.directory)}
                            >
                              <IconWindowOpen size={14} />
                              {openingIde ? "Opening IDE..." : "Open in IDE"}
                            </button>
                            {launchedIde && (
                              <span className="bc-caption">
                                Opened in <strong>{launchedIde}</strong>
                              </span>
                            )}
                            {ideError && (
                              <span
                                className="bc-caption"
                                style={{ color: "var(--accent-danger)" }}
                              >
                                {ideError}
                              </span>
                            )}
                          </div>
                          {selectedIde === CUSTOM_IDE_OPTION && (
                            <input
                              type="text"
                              data-testid="custom-ide-input"
                              className="bc-input"
                              placeholder="App name, .app path, or CLI binary (e.g. Cursor, PyCharm, code, subl)"
                              value={customIdeCommand}
                              onChange={(e) => handleChangeCustomIde(e.target.value)}
                            />
                          )}
                        </div>
                      )}
                    </div>
                  );
                })()}

                {(health?.mode === "cloud" || health?.mode === "cloud-web") && (
                  <div className="bc-card" data-testid="developer-publish-panel">
                    <div className="bc-card-header" style={{ marginBottom: 8 }}>
                      <div>
                        <span className="bc-card-title">
                          Publish Signed OCI Module to Compass Hub
                        </span>
                        <p className="bc-caption" style={{ marginTop: 2 }}>
                          Pushes digest-pinned OCI layers + static sandboxed UI assets and submits
                          for private org use or public review.
                        </p>
                      </div>
                    </div>
                    <pre className="bc-code-pre mono" style={{ fontSize: 11.5 }}>
                      {`biocompass publish-plugin --dir ./${studioPluginId.split("/")[1] || "my-module"} \\\n  --hub-url ${typeof window !== "undefined" ? window.location.origin : "http://127.0.0.1:18100"} \\\n  --visibility public_pending_review`}
                    </pre>
                    {orgPublishedPlugins.length > 0 && (
                      <div className="table-scroll" style={{ marginTop: 10 }}>
                        <table className="data-table">
                          <thead>
                            <tr>
                              <th>Module</th>
                              <th>Version</th>
                              <th>Visibility</th>
                              <th>Curated</th>
                            </tr>
                          </thead>
                          <tbody>
                            {orgPublishedPlugins.map((pv) => (
                              <tr key={`${pv.plugin_id}@${pv.version}`}>
                                <td className="mono">{pv.plugin_id}</td>
                                <td className="mono">{pv.version}</td>
                                <td>
                                  <span className="bc-badge mono">{pv.visibility}</span>
                                </td>
                                <td>
                                  {pv.is_curated_approved ? (
                                    <span className="bc-badge bc-badge-success">Approved</span>
                                  ) : (
                                    <span className="bc-caption">Standard</span>
                                  )}
                                </td>
                              </tr>
                            ))}
                          </tbody>
                        </table>
                      </div>
                    )}
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
