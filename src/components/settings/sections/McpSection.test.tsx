// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import i18next from "i18next";
import { I18nextProvider } from "react-i18next";
import { useState } from "react";
import en from "@/locales/en.json";
import { ProfilesProvider } from "@/contexts/ProfilesContext";
import { McpSection } from "./McpSection";
import { SettingsSidebarSection } from "../SettingsSidebarSection";
import type { SettingsSectionId } from "../common/SettingsNav";

const native = vi.hoisted(() => ({ invoke: vi.fn(), writeText: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: native.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn().mockResolvedValue(undefined) }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({ writeText: native.writeText }));

interface Grant {
  id: string; name: string; profile_ids: string[]; profile_created_at: Record<string, string>;
  scopes: string[]; revoked_at: string | null; last_activity_at: string | null; generation: number; rotating: boolean;
}
let profiles: { id: string; name: string; createdAt: string }[];
let status: { enabled: boolean; runtime_running: boolean; initialization_error: string | null; connections: Grant[] };
let failCommand: string | undefined;
const calls: { command: string; args?: Record<string, unknown> }[] = [];
const clientConfig = { mcpServers: { "My assistant": { command: "C:\\Apps\\Lexena\\lexena-mcp.exe", args: ["--connection", "connection-1"] } } };
const locale = i18next.createInstance();
await locale.init({ lng: "en", resources: { en: { translation: en } }, interpolation: { escapeValue: false } });

beforeEach(() => {
  vi.clearAllMocks();
  calls.length = 0;
  Object.defineProperty(navigator, "userAgent", { configurable: true, value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" });
  profiles = [{ id: "studio", name: "Studio", createdAt: "2026-01-01" }];
  status = { enabled: false, runtime_running: false, initialization_error: null, connections: [] };
  failCommand = undefined;
  native.writeText.mockResolvedValue(undefined);
  native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args });
    if (command === failCommand) throw new Error("Native service unavailable");
    switch (command) {
      case "list_profiles": return profiles;
      case "get_active_profile": return profiles[0]?.id ?? "";
      case "get_profile_avatar": return null;
      case "mcp_get_status": return structuredClone(status);
      case "mcp_set_enabled": status.enabled = args?.enabled as boolean; status.runtime_running = status.enabled; return;
      case "mcp_create_connection": {
        const grant: Grant = {
          id: "connection-1", name: args?.name as string, profile_ids: args?.profileIds as string[],
          profile_created_at: Object.fromEntries(profiles.map(p => [p.id, p.createdAt])), scopes: args?.scopes as string[],
          revoked_at: null, last_activity_at: null, generation: 0, rotating: false,
        };
        status.connections.push(grant); return grant;
      }
      case "mcp_revoke_connection": status.connections[0].revoked_at = "2026-11-05"; return;
      case "mcp_rotate_connection": status.connections[0].generation++; status.connections[0].rotating = false; return;
      case "mcp_get_client_config": return clientConfig;
      case "mcp_test_connection": return;
      case "mcp_get_audit": return [{ timestamp: "2026-11-05T12:00:00Z", connection_id: "connection-1", tool: "get_note", profile_id: "studio", object_id: "note-1", result: "OK", duration_ms: 8, operation_id: null }];
      case "mcp_reset_installation": status = { enabled: false, runtime_running: false, initialization_error: null, connections: [] }; return;
      default: throw new Error(`Unexpected command ${command}`);
    }
  });
});
afterEach(cleanup);

function mount() {
  return render(<I18nextProvider i18n={locale}><ProfilesProvider><McpSection /></ProfilesProvider></I18nextProvider>);
}
async function enable() {
  fireEvent.click(await screen.findByRole("switch", { name: "Enable local MCP" }));
  await waitFor(() => expect(screen.getByRole("switch", { name: "Enable local MCP" })).toBeChecked());
}
async function create(profileName = "Studio") {
  fireEvent.change(screen.getByLabelText("Connection name"), { target: { value: "My assistant" } });
  fireEvent.click(screen.getByRole("checkbox", { name: profileName }));
  fireEvent.click(screen.getByRole("button", { name: "Create connection" }));
  return screen.findByRole("article", { name: "My assistant" });
}

describe("Local MCP settings at the native boundary", () => {
  it("starts disabled without a cloud account and does not silently select the single profile", async () => {
    mount();
    expect(await screen.findByRole("switch", { name: "Enable local MCP" })).not.toBeChecked();
    await enable();
    expect(screen.getByRole("checkbox", { name: "Studio" })).not.toBeChecked();
    expect(screen.getByRole("button", { name: "Create connection" })).toBeDisabled();
  });
  it("requires a profile even when a connection name is supplied", async () => {
    mount(); await enable();
    fireEvent.change(screen.getByLabelText("Connection name"), { target: { value: "No profile" } });
    expect(screen.getByRole("button", { name: "Create connection" })).toBeDisabled();
    expect(calls.some(c => c.command === "mcp_create_connection")).toBe(false);
  });
  it("renders ten dynamic profiles and grants only the selected profile with read by default", async () => {
    profiles = Array.from({ length: 10 }, (_, index) => ({ id: `p${index}`, name: `Project ${index}`, createdAt: "2026-01-01" }));
    mount(); await enable();
    const group = screen.getByRole("group", { name: "Authorized profiles" });
    expect(within(group).getAllByRole("checkbox")).toHaveLength(10);
    const connection = await create("Project 8");
    expect(calls.find(c => c.command === "mcp_create_connection")?.args).toEqual({ name: "My assistant", profileIds: ["p8"], scopes: ["notes.read"] });
    expect(within(connection).getByText("Project 8")).toBeInTheDocument();
    expect(within(connection).queryByText("Project 0")).not.toBeInTheDocument();
  });
  it("keeps write, delete, replace and local-only access separate and opt-in", async () => {
    mount(); await enable();
    for (const name of ["Create and edit notes and folders", "Delete notes and folders", "Replace entire note content", "Read local-only notes"]) {
      expect(screen.getByRole("checkbox", { name })).not.toBeChecked();
    }
    fireEvent.click(screen.getByRole("checkbox", { name: "Create and edit notes and folders" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Read local-only notes" }));
    const connection = await create();
    expect(calls.find(c => c.command === "mcp_create_connection")?.args?.scopes).toEqual(["notes.read", "notes.write", "notes.local_only.read"]);
    expect(within(connection).getByText("Create and edit notes and folders")).toBeInTheDocument();
  });
  it("passes separately selected delete and replace scopes", async () => {
    mount(); await enable();
    fireEvent.click(screen.getByRole("checkbox", { name: "Delete notes and folders" }));
    fireEvent.click(screen.getByRole("checkbox", { name: "Replace entire note content" }));
    await create();
    expect(calls.find(c => c.command === "mcp_create_connection")?.args?.scopes).toEqual(["notes.read", "notes.delete", "notes.replace"]);
  });
  it("copies only the native public configuration and shows import guidance", async () => {
    mount(); await enable(); const connection = await create();
    fireEvent.click(within(connection).getByRole("button", { name: "Copy configuration" }));
    await screen.findByText("Configuration copied");
    expect(JSON.parse(native.writeText.mock.calls[0][0])).toEqual(clientConfig);
    expect(native.writeText.mock.calls[0][0]).not.toMatch(/secret|token|password/i);
    expect(calls.find(c => c.command === "mcp_get_client_config")?.args).toEqual({ connectionId: "connection-1" });
    expect(screen.getByText(/import format depends on your AI client/)).toBeInTheDocument();
  });
  it("shows an actual activation failure without claiming enabled", async () => {
    failCommand = "mcp_set_enabled"; mount();
    fireEvent.click(await screen.findByRole("switch", { name: "Enable local MCP" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Native service unavailable");
    expect(screen.getByRole("switch", { name: "Enable local MCP" })).not.toBeChecked();
    expect(screen.queryByLabelText("Connection name")).not.toBeInTheDocument();
  });
  it("does not claim a connection was created when the vault rejects it", async () => {
    failCommand = "mcp_create_connection"; mount(); await enable();
    fireEvent.change(screen.getByLabelText("Connection name"), { target: { value: "My assistant" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "Studio" }));
    fireEvent.click(screen.getByRole("button", { name: "Create connection" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Native service unavailable");
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
  });
  it("revokes a named connection without another permission prompt", async () => {
    mount(); await enable(); const connection = await create();
    fireEvent.click(within(connection).getByRole("button", { name: "Revoke" }));
    await waitFor(() => expect(within(connection).getByText("Revoked")).toBeInTheDocument());
    expect(within(connection).getByRole("button", { name: "Copy configuration" })).toBeDisabled();
    expect(calls.find(c => c.command === "mcp_revoke_connection")?.args).toEqual({ connectionId: "connection-1" });
  });
  it("pauses config export and tests when local MCP is disabled", async () => {
    mount(); await enable(); const connection = await create();
    fireEvent.click(screen.getByRole("switch", { name: "Enable local MCP" }));
    await waitFor(() => expect(screen.getByRole("switch", { name: "Enable local MCP" })).not.toBeChecked());
    expect(within(connection).getByRole("button", { name: "Copy configuration" })).toBeDisabled();
    expect(within(connection).getByRole("button", { name: "Test connection" })).toBeDisabled();
    expect(screen.queryByLabelText("Connection name")).not.toBeInTheDocument();
  });
  it("tests authentication and rotates credentials with native results", async () => {
    mount(); await enable(); const connection = await create();
    fireEvent.click(within(connection).getByRole("button", { name: "Test connection" }));
    await screen.findByText("Local connection authenticated");
    fireEvent.click(within(connection).getByRole("button", { name: "Rotate credentials" }));
    await screen.findByText("Credentials rotated. Reconnect your AI client.");
    expect(calls.find(c => c.command === "mcp_rotate_connection")?.args).toEqual({ connectionId: "connection-1" });
  });
  it("displays audit metadata from the native service", async () => {
    mount(); await enable(); await create();
    fireEvent.click(screen.getByRole("button", { name: "Refresh activity" }));
    const table = await screen.findByRole("table", { name: "Local MCP activity" });
    expect(within(table).getByText("My assistant")).toBeInTheDocument();
    expect(within(table).getByText("Studio")).toBeInTheDocument();
    expect(within(table).getByText("get_note")).toBeInTheDocument();
    expect(within(table).getByText("OK")).toBeInTheDocument();
  });
  it("requires an explicit confirmation before resetting moved installation connections", async () => {
    status.initialization_error = "Installation moved";
    mount();
    expect(await screen.findByRole("alert")).toHaveTextContent("Installation moved");
    expect(screen.getByRole("switch", { name: "Enable local MCP" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Reset AI connections" }));
    const dialog = screen.getByRole("dialog", { name: "Reset AI connections?" });
    expect(within(dialog).getByText(/notes and profiles are preserved/)).toBeInTheDocument();
    expect(calls.some(c => c.command === "mcp_reset_installation")).toBe(false);
    fireEvent.click(within(dialog).getByRole("button", { name: "Reset and reconnect" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(calls.filter(c => c.command === "mcp_reset_installation")).toHaveLength(1);
    expect(screen.getByRole("switch", { name: "Enable local MCP" })).not.toBeChecked();
    expect(screen.getByText(/Create a new connection and import its configuration/)).toBeInTheDocument();
  });
  it("keeps the installation intact when reset is cancelled", async () => {
    status.initialization_error = "Installation moved"; mount();
    fireEvent.click(await screen.findByRole("button", { name: "Reset AI connections" }));
    fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(calls.some(c => c.command === "mcp_reset_installation")).toBe(false);
  });
  it("shows reset failure inside the confirmation dialog and keeps recovery available", async () => {
    status.initialization_error = "Installation moved"; failCommand = "mcp_reset_installation";
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Reset AI connections" }));
    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Reset and reconnect" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("Native service unavailable");
    expect(screen.getByRole("switch", { hidden: true })).not.toBeChecked();
    expect(within(dialog).getByRole("button", { name: "Reset and reconnect" })).toBeEnabled();
  });
  it("does not claim revocation after a native error", async () => {
    mount(); await enable(); const connection = await create();
    failCommand = "mcp_revoke_connection";
    fireEvent.click(within(connection).getByRole("button", { name: "Revoke" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Native service unavailable");
    expect(within(connection).getByText("Authorized")).toBeInTheDocument();
  });
  it("reloads a durably blocked rotation after the vault rejects it and permits retry", async () => {
    mount(); await enable(); const connection = await create();
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "mcp_rotate_connection") {
        status.connections[0].generation++;
        status.connections[0].rotating = true;
        throw new Error("Credential vault unavailable during rotation");
      }
      return original(command, args);
    });
    fireEvent.click(within(connection).getByRole("button", { name: "Rotate credentials" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Credential vault unavailable during rotation");
    expect(within(connection).queryByText("Authorized")).not.toBeInTheDocument();
    expect(within(connection).getByRole("button", { name: "Copy configuration" })).toBeDisabled();
    expect(within(connection).getByRole("button", { name: "Test connection" })).toBeDisabled();
    const retry = within(connection).getByRole("button", { name: "Retry credential rotation" });
    expect(retry).toBeEnabled();
    native.invoke.mockImplementation(original);
    fireEvent.click(retry);
    await screen.findByText("Credentials rotated. Reconnect your AI client.");
    expect(within(connection).getByText("Authorized")).toBeInTheDocument();
    expect(within(connection).getByRole("button", { name: "Test connection" })).toBeEnabled();
  });
  it("reloads a durably revoked connection after credential deletion fails", async () => {
    mount(); await enable(); const connection = await create();
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "mcp_revoke_connection") {
        status.connections[0].revoked_at = "2026-11-05";
        throw new Error("Credential vault unavailable during revocation");
      }
      return original(command, args);
    });
    fireEvent.click(within(connection).getByRole("button", { name: "Revoke" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Credential vault unavailable during revocation");
    expect(within(connection).getByText("Revoked")).toBeInTheDocument();
    expect(within(connection).queryByText("Authorized")).not.toBeInTheDocument();
    for (const name of ["Copy configuration", "Test connection", "Rotate credentials", "Revoke"]) {
      expect(within(connection).getByRole("button", { name })).toBeDisabled();
    }
  });
  it.each(["rotation", "revocation"])("discards cached authorization when %s and status recovery both fail", async action => {
    mount(); await enable(); const connection = await create();
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === (action === "rotation" ? "mcp_rotate_connection" : "mcp_revoke_connection")) {
        if (action === "rotation") status.connections[0].rotating = true;
        else status.connections[0].revoked_at = "2026-11-05";
        failCommand = "mcp_get_status";
        throw new Error(`Credential vault unavailable during ${action}`);
      }
      if (command === "mcp_get_status" && failCommand) throw new Error("Status recovery unavailable");
      return original(command, args);
    });
    fireEvent.click(within(connection).getByRole("button", { name: action === "rotation" ? "Rotate credentials" : "Revoke" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(`Credential vault unavailable during ${action}`);
    expect(screen.queryByText("Status recovery unavailable")).not.toBeInTheDocument();
    expect(screen.queryByText("Authorized")).not.toBeInTheDocument();
    expect(screen.queryByRole("article")).not.toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Enable local MCP" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Copy configuration" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Test connection" })).not.toBeInTheDocument();
    failCommand = undefined;
    fireEvent.click(screen.getByRole("button", { name: "Refresh status" }));
    const recovered = await screen.findByRole("article", { name: "My assistant" });
    expect(within(recovered).getByRole("button", { name: "Copy configuration" })).toBeDisabled();
    expect(within(recovered).getByRole("button", { name: "Test connection" })).toBeDisabled();
    if (action === "rotation") expect(within(recovered).getByRole("button", { name: "Retry credential rotation" })).toBeEnabled();
    else expect(within(recovered).getByText("Revoked")).toBeInTheDocument();
  });
  it("does not copy or report success when configuration export fails", async () => {
    mount(); await enable(); const connection = await create();
    failCommand = "mcp_get_client_config";
    fireEvent.click(within(connection).getByRole("button", { name: "Copy configuration" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Native service unavailable");
    expect(native.writeText).not.toHaveBeenCalled();
    expect(screen.queryByText("Configuration copied")).not.toBeInTheDocument();
  });
  it("excludes metadata and environment credentials from clipboard export", async () => {
    const original = native.invoke.getMockImplementation()!;
    native.invoke.mockImplementation(async (command: string, args?: Record<string, unknown>) => command === "mcp_get_client_config"
      ? { secret: "private-value", mcpServers: { "My assistant": { ...clientConfig.mcpServers["My assistant"], env: { TOKEN: "private-value" } } } }
      : original(command, args));
    mount(); await enable(); const connection = await create();
    fireEvent.click(within(connection).getByRole("button", { name: "Copy configuration" }));
    await screen.findByText("Configuration copied");
    expect(JSON.parse(native.writeText.mock.calls[0][0])).toEqual(clientConfig);
    expect(native.writeText.mock.calls[0][0]).not.toContain("private-value");
  });
  it("blocks a connection during incomplete rotation and allows an explicit retry", async () => {
    status.enabled = true; status.runtime_running = true;
    status.connections = [{ id: "connection-1", name: "My assistant", profile_ids: ["studio"], profile_created_at: { studio: "2026-01-01" }, scopes: ["notes.read"], revoked_at: null, last_activity_at: null, generation: 1, rotating: true }];
    mount(); const connection = await screen.findByRole("article", { name: "My assistant" });
    expect(within(connection).getByRole("button", { name: "Test connection" })).toBeDisabled();
    expect(within(connection).getByRole("button", { name: "Copy configuration" })).toBeDisabled();
    fireEvent.click(within(connection).getByRole("button", { name: "Retry credential rotation" }));
    await screen.findByText("Credentials rotated. Reconnect your AI client.");
    expect(within(connection).getByRole("button", { name: "Test connection" })).toBeEnabled();
  });
  it("offers integrations in the real sidebar without an auth or cloud provider", async () => {
    function SettingsScreen() {
      const [active, setActive] = useState<SettingsSectionId>("section-dictee");
      return <><SettingsSidebarSection activeSection={active} onSectionChange={setActive} />{active === "section-integrations" && <McpSection />}</>;
    }
    render(<I18nextProvider i18n={locale}><ProfilesProvider><SettingsScreen /></ProfilesProvider></I18nextProvider>);
    const integrations = screen.getByRole("button", { name: /Integrations Local MCP and AI connections/ });
    fireEvent.click(integrations);
    expect(integrations).toHaveAttribute("aria-current", "page");
    expect(await screen.findByRole("switch", { name: "Enable local MCP" })).not.toBeChecked();
  });
  it("never labels a recreated profile as authorized through an old grant", async () => {
    status.connections = [{ id: "connection-1", name: "Old client", profile_ids: ["studio"], profile_created_at: { studio: "2025-01-01" }, scopes: ["notes.read"], revoked_at: null, last_activity_at: "2026-11-04T10:00:00Z", generation: 0, rotating: false }];
    mount();
    const connection = await screen.findByRole("article", { name: "Old client" });
    expect(within(connection).queryByText("Studio")).not.toBeInTheDocument();
    expect(within(connection).getByText("Profile unavailable — create a new connection")).toBeInTheDocument();
  });
  it("shows unsupported platforms without invoking Windows MCP commands", async () => {
    Object.defineProperty(navigator, "userAgent", { configurable: true, value: "Mozilla/5.0 (Macintosh; Intel Mac OS X)" });
    mount();
    expect(await screen.findByText("Local MCP is currently available on Windows only.")).toBeInTheDocument();
    expect(calls.some(c => c.command.startsWith("mcp_"))).toBe(false);
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
  });
});
