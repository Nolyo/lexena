/** Public UI command shapes mirror commands/mcp.rs and mcp/{connections,audit}.rs. */
export type McpScope =
  | "notes.read"
  | "notes.write"
  | "notes.delete"
  | "notes.replace"
  | "notes.local_only.read";

export interface McpConnection {
  id: string;
  name: string;
  profile_ids: string[];
  /** A recreated profile ID must not inherit the prior profile's authorization. */
  profile_created_at: Record<string, string>;
  scopes: McpScope[];
  revoked_at: string | null;
  last_activity_at: string | null;
  generation: number;
  rotating: boolean;
}

export interface McpStatus {
  initialization_error: string | null;
  enabled: boolean;
  runtime_running: boolean;
  connections: McpConnection[];
}

export interface McpClientConfig {
  mcpServers: Record<string, { command: string; args: string[] }>;
}

export type McpAuditTool =
  | "get_context" | "list_profiles" | "list_folders" | "list_notes"
  | "search_notes" | "get_note" | "create_note" | "append_note"
  | "edit_note" | "replace_note" | "move_note" | "delete_note"
  | "restore_note" | "list_history" | "create_folder" | "rename_folder" | "delete_folder";

export interface McpAuditEntry {
  timestamp: string;
  connection_id: string;
  tool: McpAuditTool;
  profile_id: string | null;
  object_id: string | null;
  result: "OK" | "DENIED" | "CONFLICT" | "CANCELLED" | "FAILED";
  duration_ms: number;
  operation_id: string | null;
}
