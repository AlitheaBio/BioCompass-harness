import type { DatasetGrant } from "./types";

/**
 * Enforce active `DatasetGrant` boundaries on `biocompass.queryCatalog` bridge calls (`SEC-22`).
 * Rejects any non-empty `additional_sources` attempting to escape the active grant or catalog,
 * and always strips `additional_sources` before forwarding to the host API.
 */
export function enforcePluginQueryGrant<T extends Record<string, unknown>>(
  grant: DatasetGrant | null,
  query: T,
): T {
  if (!grant) {
    throw new Error("Select a dataset grant with row access before querying a catalog.");
  }
  if (grant.level !== "rows") {
    throw new Error(
      `Grant '${grant.label}' is '${grant.level}' — direct catalog row queries require level 'rows'. Run a curated module instead.`,
    );
  }
  const extra = (query as { additional_sources?: unknown }).additional_sources;
  if (Array.isArray(extra) && extra.length > 0) {
    throw new Error(
      "FORBIDDEN: query.additional_sources outside the active DatasetGrant is not permitted from sandboxed plugin frames.",
    );
  }
  if (extra !== undefined && !Array.isArray(extra)) {
    throw new Error("FORBIDDEN: invalid query.additional_sources parameter.");
  }
  const sanitized = { ...query };
  delete sanitized["additional_sources"];
  return sanitized;
}

/**
 * Enforce `allow_local_export` on `biocompass.downloadFile` bridge calls (`SEC-22`).
 */
export function enforcePluginDownloadPolicy(
  grant: DatasetGrant | null,
  filename: string,
): { safeFilename: string } {
  if (grant && grant.allow_local_export === false) {
    throw new Error(
      `FORBIDDEN: local export is disabled by active DatasetGrant '${grant.label}' (allow_local_export=false).`,
    );
  }
  const base = filename
    .split(/[\\/]/)
    .pop()
    ?.replace(/[^\w.\-() ]+/g, "_")
    .trim();
  const safeFilename = !base || base === "." || base === ".." ? "download.bin" : base.slice(0, 128);
  return { safeFilename };
}

export interface StagedPluginChatDraft {
  pluginId: string;
  label: string;
  preview?: string;
  defaultMessage: string;
  context?: Record<string, unknown>;
  stagedComposerText: string;
  autoSubmit: false;
}

/**
 * Stage `biocompass.sendToChat` into the chat composer for explicit user confirmation (`SEC-27`).
 * NEVER auto-submits an LLM turn regardless of `context.mode`.
 */
export function buildStagedPluginChatDraft(
  pluginId: string,
  message: string,
  context?: Record<string, unknown>,
  existingComposerInput = "",
): StagedPluginChatDraft {
  const label =
    context && typeof context["attachment_label"] === "string" && context["attachment_label"].trim()
      ? context["attachment_label"].trim()
      : pluginId;
  const preview =
    context && typeof context["attachment_preview"] === "string"
      ? context["attachment_preview"]
      : undefined;
  const trimmedMsg = message.trim();
  const stagedComposerText = existingComposerInput.trim()
    ? existingComposerInput
    : trimmedMsg;
  return {
    pluginId,
    label,
    preview,
    defaultMessage: trimmedMsg,
    context,
    stagedComposerText,
    autoSubmit: false,
  };
}
