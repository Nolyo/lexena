import { invoke } from "@tauri-apps/api/core";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import type { McpAuditEntry, McpClientConfig, McpConnection, McpScope, McpStatus } from "./types";

/** Local main-window administration only. These commands never expose vault secrets. */
export const mcpClient = {
  getStatus: () => invoke<McpStatus>("mcp_get_status"),
  setEnabled: (enabled: boolean) => invoke<void>("mcp_set_enabled", { enabled }),
  createConnection: (name: string, profileIds: string[], scopes: McpScope[]) =>
    invoke<McpConnection>("mcp_create_connection", { name, profileIds, scopes }),
  revokeConnection: (connectionId: string) => invoke<void>("mcp_revoke_connection", { connectionId }),
  rotateConnection: (connectionId: string) => invoke<void>("mcp_rotate_connection", { connectionId }),
  testConnection: (connectionId: string) => invoke<void>("mcp_test_connection", { connectionId }),
  getAudit: () => invoke<McpAuditEntry[]>("mcp_get_audit"),
  resetInstallation: () => invoke<void>("mcp_reset_installation"),
  async copyClientConfig(connectionId: string): Promise<McpClientConfig> {
    const config = await invoke<McpClientConfig>("mcp_get_client_config", { connectionId });
    // Only public launch fields belong in a client export, even if the native
    // response later grows additional metadata. Never export env or credentials.
    const publicConfig: McpClientConfig = {
      mcpServers: Object.fromEntries(Object.entries(config.mcpServers).map(([name, server]) => [
        name, { command: server.command, args: server.args },
      ])),
    };
    await writeText(JSON.stringify(publicConfig, null, 2));
    return publicConfig;
  },
};
