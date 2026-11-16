import type {
  CatalogManifest,
  CatalogQuery,
  CatalogQueryResult,
  DatasetGrant,
  FrogletVerifyReport,
  HealthStatus,
  LedgerState,
  PluginSummary,
  RunLogEvent,
  RunQuote,
  RunRecord,
  ScaffoldResult,
  SessionEvent,
  SessionRecord,
} from "./types";

async function apiFetch<T>(url: string, init?: RequestInit): Promise<T> {
  const response = await fetch(url, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  if (!response.ok) {
    let errText = `${response.status} ${response.statusText}`;
    try {
      const body = (await response.json()) as { error?: string };
      if (body.error) {
        errText = body.error;
      }
    } catch {
      // keep status text
    }
    throw new Error(errText);
  }
  return (await response.json()) as T;
}

export async function fetchHealth(): Promise<HealthStatus> {
  return apiFetch<HealthStatus>("/api/v1/health");
}

export async function fetchGrants(): Promise<DatasetGrant[]> {
  const data = await apiFetch<{ grants: DatasetGrant[] }>("/api/v1/grants");
  return data.grants;
}

export async function fetchCatalogs(): Promise<CatalogManifest[]> {
  const data = await apiFetch<{ catalogs: CatalogManifest[] }>("/api/v1/catalogs");
  return data.catalogs;
}

export async function queryCatalog(
  catalogId: string,
  grantId: string,
  query: CatalogQuery,
): Promise<CatalogQueryResult> {
  const [ns, name] = catalogId.split("/");
  if (!ns || !name) {
    throw new Error(`Invalid catalog ID: ${catalogId}`);
  }
  return apiFetch<CatalogQueryResult>(`/api/v1/catalogs/${ns}/${name}/query`, {
    method: "POST",
    body: JSON.stringify({ grant_id: grantId, query }),
  });
}

export async function fetchPlugins(): Promise<PluginSummary[]> {
  const data = await apiFetch<{ plugins: PluginSummary[] }>("/api/v1/plugins");
  return data.plugins;
}

export async function scaffoldPlugin(payload: {
  template: string;
  plugin_id: string;
  display_name: string;
  include_vite_ui: boolean;
}): Promise<ScaffoldResult> {
  return apiFetch<ScaffoldResult>("/api/v1/plugins/scaffold", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function fetchRuns(): Promise<RunRecord[]> {
  const data = await apiFetch<{ runs: RunRecord[] }>("/api/v1/runs");
  return data.runs;
}

export async function prepareRun(payload: {
  plugin_id: string;
  params: Record<string, unknown>;
  grant_id?: string;
  session_id?: string;
}): Promise<RunQuote> {
  return apiFetch<RunQuote>("/api/v1/runs/prepare", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function confirmRun(
  runId: string,
  idempotencyKey?: string,
): Promise<RunRecord> {
  return apiFetch<RunRecord>(`/api/v1/runs/${encodeURIComponent(runId)}/confirm`, {
    method: "POST",
    body: JSON.stringify({ idempotency_key: idempotencyKey }),
  });
}

export function subscribeRunLogs(
  runId: string,
  onLog: (ev: RunLogEvent) => void,
): () => void {
  const es = new EventSource(`/api/v1/runs/${encodeURIComponent(runId)}/events`);
  es.addEventListener("log", (ev: MessageEvent<string>) => {
    try {
      const parsed = JSON.parse(ev.data) as RunLogEvent;
      onLog(parsed);
    } catch {
      // ignore malformed keepalive
    }
  });
  return () => {
    es.close();
  };
}

export async function verifyFrogletChain(
  documents: Record<string, unknown>[],
): Promise<FrogletVerifyReport> {
  return apiFetch<FrogletVerifyReport>("/api/v1/froglet/verify", {
    method: "POST",
    body: JSON.stringify({ documents }),
  });
}

export async function fetchLedger(): Promise<LedgerState> {
  return apiFetch<LedgerState>("/api/v1/ledger");
}

export async function fetchSessions(): Promise<SessionRecord[]> {
  const data = await apiFetch<{ sessions: SessionRecord[] }>("/api/v1/sessions");
  return data.sessions;
}

export async function createSession(
  title: string,
  activeGrantId: string,
): Promise<SessionRecord> {
  return apiFetch<SessionRecord>("/api/v1/sessions", {
    method: "POST",
    body: JSON.stringify({ title, active_grant_id: activeGrantId }),
  });
}

export async function updateSessionGrant(
  sessionId: string,
  activeGrantId: string,
): Promise<SessionRecord> {
  return apiFetch<SessionRecord>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}`,
    {
      method: "PATCH",
      body: JSON.stringify({ active_grant_id: activeGrantId }),
    },
  );
}

export async function fetchSessionEvents(sessionId: string): Promise<SessionEvent[]> {
  const data = await apiFetch<{ events: SessionEvent[] }>(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/events`,
  );
  return data.events;
}

export interface AgentSseEnvelope {
  type: "text_delta" | "tool_call" | "tool_result" | "run_prepared" | "completed" | "error";
  data: unknown;
}

export async function streamAgentChatTurn(
  sessionId: string,
  message: string,
  onEvent: (ev: AgentSseEnvelope) => void,
): Promise<void> {
  const response = await fetch(
    `/api/v1/sessions/${encodeURIComponent(sessionId)}/chat`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ message }),
    },
  );
  if (!response.ok || !response.body) {
    throw new Error(`Chat SSE failed: ${response.status}`);
  }
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    let boundary = buffer.indexOf("\n\n");
    while (boundary !== -1) {
      const chunk = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      boundary = buffer.indexOf("\n\n");

      for (const line of chunk.split("\n")) {
        if (line.startsWith("data:")) {
          const rawJson = line.slice(5).trim();
          if (rawJson) {
            const parsed = JSON.parse(rawJson) as AgentSseEnvelope;
            onEvent(parsed);
          }
        }
      }
    }
  }
}
