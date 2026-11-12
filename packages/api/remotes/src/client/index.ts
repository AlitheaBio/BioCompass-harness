/** Platform-neutral assembly of generated Host Remote contributions. */

import type { Context } from '@deepseek-ai/cordis'
import productAnalyticsRemote from '@deepseek-ai/dsh-client-product-analytics/remote'
export type {} from '@deepseek-ai/dsh-client-product-analytics/remote'
import agentPresetsRemote from '@deepseek-ai/dsh-agent-preset-registry/remote'
import userQuestionsRemote from '@deepseek-ai/dsh-user-questions/remote'
import commandsRemote from '@deepseek-ai/dsh-commands/remote'
import agentTeamsRemote from '@deepseek-ai/dsh-agent-team/remote'
export type {} from '@deepseek-ai/dsh-agent-team/remote'
import settingsControllerRemote from '@deepseek-ai/dsh-api-settings-controller/remote'
import goalsRemote from '@deepseek-ai/dsh-goal/remote'
import scheduleRemote from '@deepseek-ai/dsh-schedule/remote'
import llmRemote from '@deepseek-ai/dsh-llm/remote'
import pluginManagerRemote from '@deepseek-ai/dsh-plugin-manager/remote'
import mcpManagerRemote from '@deepseek-ai/dsh-mcp-manager/remote'
import toolResultsRemote from '@deepseek-ai/dsh-client-ui-trajectory/remote'
import pluginRegistryProbeRemote from '@deepseek-ai/dsh-client-ui-plugin-manager/remote'
import pluginInventoryRemote from '@deepseek-ai/dsh-host-plugin-inventory/remote'
import permissionPresetsRemote from '@deepseek-ai/dsh-permission-presets/remote'
import fileUploadsRemote from '@deepseek-ai/dsh-client-file-upload/remote'
import sessionReferencesRemote from '@deepseek-ai/dsh-session-reference/remote'
import subagentsRemote from '@deepseek-ai/dsh-subagent/remote'
import sessionRemote from '@deepseek-ai/dsh-api-session-controller/remote'
import jobRemote from '@deepseek-ai/dsh-api-job-controller/remote'
import workspaceRemote from '@deepseek-ai/dsh-api-workspace-controller/remote'
import terminalRemote from '@deepseek-ai/dsh-api-terminal-controller/remote'
import workspaceFilesRemote from '@deepseek-ai/dsh-api-workspace-files/remote'
import webRemote from '@deepseek-ai/dsh-web/remote'
import type { ClientRemote } from '@deepseek-ai/dsh-api-gateway/client'

export type { ClientRemote } from '@deepseek-ai/dsh-api-gateway/client'
export type {
  BundleDetail, BundleInfo, BundleRowInfo, ChangeResult, IncompatiblePlugin, InspectOptions, InstallBundleOptions, InstallSpecKind,
  ManagementError, PackageResult,
  PluginChange, PluginEntryId, PluginInfo, PluginInspectProblem, PluginInstallCancellation, PluginInstallFailureKind,
  PluginInstallLogChunk, PluginInstallProgress, PluginInstallRequestId, PluginRegistries, PluginSpecInspection,
  PluginUpdateProgress, PluginUpdateResult, PluginUpdateStep, PluginVersion, PluginVersionList, RegistryAttempt,
  ReadOnlyReason, Registry, RestartReason,
} from '@deepseek-ai/dsh-plugin-manager/types'
export type { McpManagedServerStatus, McpManagedServerView } from '@deepseek-ai/dsh-mcp-manager/types'
export type {} from '@deepseek-ai/dsh-plugin-manager/remote'
export type {} from '@deepseek-ai/dsh-mcp-manager/remote'
export type {} from '@deepseek-ai/dsh-client-ui-plugin-manager/remote'
export type {} from '@deepseek-ai/dsh-client-ui-trajectory/remote'
export type { PluginInventorySnapshot } from '@deepseek-ai/dsh-host-plugin-inventory/types'
export type {} from '@deepseek-ai/dsh-agent-preset-registry/remote'
export type {} from '@deepseek-ai/dsh-user-questions/remote'
export type {} from '@deepseek-ai/dsh-commands/remote'
export type {} from '@deepseek-ai/dsh-web/remote'
export type {} from '@deepseek-ai/dsh-api-settings-controller/remote'
export type {} from '@deepseek-ai/dsh-goal/remote'
export type {} from '@deepseek-ai/dsh-schedule/remote'
export type {} from '@deepseek-ai/dsh-llm/remote'
export type {} from '@deepseek-ai/dsh-host-plugin-inventory/remote'
export type {} from '@deepseek-ai/dsh-permission-presets/remote'
export type {} from '@deepseek-ai/dsh-client-file-upload/remote'
export type {} from '@deepseek-ai/dsh-session-reference/remote'
export type {} from '@deepseek-ai/dsh-subagent/remote'
export type * from '@deepseek-ai/dsh-subagent/client'
export type {} from '@deepseek-ai/dsh-api-session-controller/remote'
export type * from '@deepseek-ai/dsh-api-session-controller/types'
export type {} from '@deepseek-ai/dsh-api-job-controller/remote'
export type * from '@deepseek-ai/dsh-api-job-controller/types'
export type {} from '@deepseek-ai/dsh-api-workspace-controller/remote'
export type * from '@deepseek-ai/dsh-api-workspace-controller/types'
export type {} from '@deepseek-ai/dsh-api-workspace-files/remote'
export type * from '@deepseek-ai/dsh-api-workspace-files/types'
export type {} from '@deepseek-ai/dsh-api-terminal-controller/remote'
export type * from '@deepseek-ai/dsh-api-terminal-controller/types'
export type { ApiRemoteForwardedEvent } from '../types.ts'
export type {} from '@deepseek-ai/dsh-commands/types'
export type {} from '@deepseek-ai/dsh-web/types'
export type {} from '@deepseek-ai/dsh-credentials/types'
export type {} from '@deepseek-ai/dsh-llm/types'
export type {} from '@deepseek-ai/dsh-agent-preset-registry/types'
export type {} from '@deepseek-ai/dsh-permission-presets/types'
export type {} from '@deepseek-ai/dsh-settings/types'
export type {} from '@deepseek-ai/dsh-user-approval/types'
export type {} from '@deepseek-ai/dsh-user-questions/types'
export type {} from '@deepseek-ai/dsh-api-session-controller/types'

export type {
  ConnectionHandle, ConnectionSinks, ContentBlock,
  MessageId,
  RpcId, RpcRequest, RpcResponse, RpcResult, SessionId,
  StreamChunk,
} from '@deepseek-ai/dsh-client-connection/client'
export type {} from '@deepseek-ai/dsh-api-gateway/client'
export type { CredentialInfo } from '@deepseek-ai/dsh-credentials/types'
export type {
  SettingsDescribeValue, SettingsNamespaceView, SettingsPathOpView, SettingsSecretView,
} from '@deepseek-ai/dsh-settings/types'
export type {
  LlmConfigurableProvider, LlmDiscoveredModel,
  LlmModelDiscoveryRequest, LlmNativeAuth, LlmProviderInfo,
} from '@deepseek-ai/dsh-llm/types'
export type { FileReferenceCandidate } from '@deepseek-ai/dsh-file-reference/types'
export type { SessionReferenceMentionCandidate } from '@deepseek-ai/dsh-session-reference/types'
export type {
  RemoteErrorCode, RemoteErrorDetailsMap, RemoteFailure, RemoteResult,
} from '@deepseek-ai/dsh-typert-protocol'
export type { RemoteHostFacts } from '@deepseek-ai/dsh-api-gateway/client'

declare module '@deepseek-ai/cordis' {
  interface Context {
    /** Generated Remote namespaces selected by this Client assembly. */
    remote: ClientRemote
  }
}

/** Required service: the typed Client Remote contribution mount. */
export const inject = ['remote']

/**
 * Mount the Host capabilities explicitly selected for this Client assembly.
 * @param ctx - Client Cordis root carrying the typed API service.
 * @returns disposer after every selected Remote namespace is ready.
 */
export async function apply(ctx: Context): Promise<() => Promise<void>> {
  const disposers: Array<() => Promise<void>> = []
  try {
    for (const contribution of [
      productAnalyticsRemote, agentPresetsRemote, commandsRemote, agentTeamsRemote, settingsControllerRemote,
      goalsRemote, llmRemote, scheduleRemote,
      pluginInventoryRemote, pluginManagerRemote, mcpManagerRemote, pluginRegistryProbeRemote,
      toolResultsRemote,
      fileUploadsRemote, sessionReferencesRemote,
      permissionPresetsRemote, subagentsRemote, sessionRemote, jobRemote, workspaceRemote, workspaceFilesRemote, terminalRemote,
      userQuestionsRemote, webRemote,
    ]) {
      disposers.push(await ctx.remote.$mount(contribution))
    }
  } catch (error) {
    for (const dispose of disposers.reverse()) await dispose()
    throw error
  }
  return async () => {
    for (const dispose of disposers.reverse()) await dispose()
  }
}
