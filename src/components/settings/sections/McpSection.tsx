import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useProfiles } from "@/contexts/ProfilesContext";
import { useMcpSettings } from "@/hooks/useMcpSettings";
import { mcpClient } from "@/lib/mcp/client";
import type { McpScope } from "@/lib/mcp/types";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Callout, SectionHeader, VtIcon } from "../vt";

const SCOPE_KEYS: Record<McpScope, string> = {
  "notes.read": "read", "notes.write": "write", "notes.delete": "delete",
  "notes.replace": "replace", "notes.local_only.read": "localOnly",
};
const OPTIONAL_SCOPES: McpScope[] = ["notes.write", "notes.delete", "notes.replace", "notes.local_only.read"];

export function McpSection() {
  const { t, i18n } = useTranslation();
  const { profiles, isLoaded: profilesLoaded } = useProfiles();
  const mcp = useMcpSettings();
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [scopes, setScopes] = useState<McpScope[]>(["notes.read"]);
  const [message, setMessage] = useState<string | null>(null);
  const [resetOpen, setResetOpen] = useState(false);
  const [configPreview, setConfigPreview] = useState<string | null>(null);
  const status = mcp.status;
  const disabled = mcp.busy || mcp.loading || !status || !!status.initialization_error;
  const selectedProfiles = profiles.filter(p => selected.includes(p.id));
  const canCreate = !disabled && profilesLoaded && status?.enabled && name.trim().length > 0
    && new TextEncoder().encode(name.trim()).length <= 128 && selectedProfiles.length > 0;
  const scopeLabel = (scope: McpScope) => t(`mcp.scopes.${SCOPE_KEYS[scope]}`);
  const date = (value: string) => new Date(value).toLocaleString(i18n.language);

  async function act(operation: () => Promise<unknown>, success?: string) {
    setMessage(null);
    const result = await mcp.execute(operation);
    if (result.ok && success) setMessage(success);
    return result.ok;
  }

  async function create(event: FormEvent) {
    event.preventDefault();
    if (!canCreate) return;
    if (await act(() => mcpClient.createConnection(name.trim(), selectedProfiles.map(p => p.id), scopes))) {
      setName(""); setSelected([]); setScopes(["notes.read"]);
    }
  }

  return (
    <div id="section-integrations" className="space-y-5">
      <section id="section-mcp" className="vt-card-sectioned overflow-hidden">
        <SectionHeader color="var(--vt-accent)" icon={<VtIcon.plug />}
          title={t("mcp.title")} description={t("mcp.subtitle")}
          trailing={mcp.supported && <Button variant="ghost" size="sm" disabled={mcp.busy} onClick={() => { setMessage(null); void mcp.refresh(); }}>{t("mcp.refresh")}</Button>} />
        <div className="space-y-5 p-5 text-[13px]">
          <p className="max-w-prose text-muted-foreground">{t("mcp.intro")}</p>
          {!mcp.supported ? <Callout>{t("mcp.unsupported")}</Callout> : <>
            {mcp.error && !resetOpen && <div role="alert"><Callout kind="danger">{mcp.error}</Callout></div>}
            {status?.initialization_error && <div role="alert"><Callout kind="warn" title={t("mcp.reconnectTitle")}>
              <p>{t("mcp.reconnectHelp")}</p>
              <p className="mt-2 break-words">{status.initialization_error}</p>
              <Button className="mt-3" variant="outline" size="sm" disabled={mcp.busy} onClick={() => setResetOpen(true)}>{t("mcp.reset")}</Button>
            </Callout></div>}
            {message && <p role="status" className="text-vt-green">{message}</p>}
            <div className="flex flex-wrap items-center justify-between gap-4 border-b border-border pb-5">
              <div>
                <p className="font-medium">{t("mcp.enable")}</p>
                <p className="mt-1 text-xs text-muted-foreground">{mcp.loading ? t("mcp.loading") : !status ? t("mcp.unavailable") : t(status.enabled ? status.runtime_running ? "mcp.running" : "mcp.stopped" : "mcp.disabled")}</p>
              </div>
              <button type="button" role="switch" aria-checked={status?.enabled ?? false} aria-label={t("mcp.enable")}
                className="rounded-md p-2 focus-visible:outline-2 focus-visible:outline-ring disabled:opacity-50"
                disabled={disabled} onClick={() => { void act(() => mcpClient.setEnabled(!status?.enabled)); }}>
                <span className="vt-toggle block" data-on={status?.enabled ?? false} />
              </button>
            </div>
            <Callout title={t("mcp.localTitle")}>
              <p>{t("mcp.localHelp")}</p>
              <p className="mt-2">{t("mcp.modelHelp")}</p>
            </Callout>
            {status?.enabled && !status.initialization_error && <form onSubmit={create} className="space-y-4">
              <h3 className="font-semibold">{t("mcp.connect")}</h3>
              <label className="flex flex-col gap-2 max-w-md">
                <span>{t("mcp.name")}</span>
                <input className="h-9 rounded-md border border-border bg-background px-3 focus-visible:outline-2 focus-visible:outline-ring"
                  value={name} onChange={e => setName(e.target.value)} disabled={mcp.busy} maxLength={128} />
              </label>
              <p className="text-xs text-muted-foreground">{t("mcp.nameHelp")}</p>
              <fieldset disabled={mcp.busy || !profilesLoaded}>
                <legend className="mb-2 font-medium">{t("mcp.profiles")}</legend>
                <div className="max-h-60 overflow-y-auto space-y-2 rounded-md border border-border p-3">
                  {profiles.map(profile => <label key={profile.id} className="flex items-center gap-2 break-words">
                    <input type="checkbox" className="accent-primary shrink-0" checked={selected.includes(profile.id)}
                      onChange={e => setSelected(current => e.target.checked ? [...current, profile.id] : current.filter(id => id !== profile.id))} />
                    {profile.name}
                  </label>)}
                  {!profiles.length && <p className="text-muted-foreground">{t("mcp.noProfiles")}</p>}
                </div>
                <p className="mt-2 text-xs text-muted-foreground">{t("mcp.profileHelp")}</p>
              </fieldset>
              <fieldset disabled={mcp.busy} className="space-y-2">
                <legend className="mb-2 font-medium">{t("mcp.permissions")}</legend>
                <label className="flex items-center gap-2"><input type="checkbox" checked disabled className="accent-primary" />{scopeLabel("notes.read")}</label>
                {OPTIONAL_SCOPES.map(scope => <label key={scope} className="flex items-center gap-2">
                  <input type="checkbox" checked={scopes.includes(scope)} className="accent-primary"
                    onChange={e => setScopes(current => e.target.checked ? [...current, scope] : current.filter(s => s !== scope))} />
                  {scopeLabel(scope)}
                </label>)}
                <p className="text-xs text-muted-foreground">{t("mcp.permissionHelp")}</p>
              </fieldset>
              <Button type="submit" size="sm" disabled={!canCreate}>{t("mcp.create")}</Button>
            </form>}
            <section className="space-y-3" aria-label={t("mcp.connections")}>
              <h3 className="font-semibold">{t("mcp.connections")}</h3>
              {status && !status.connections.length && <p className="text-muted-foreground">{t("mcp.empty")}</p>}
              {status?.connections.map(connection => {
                const granted = profiles.filter(p => connection.profile_ids.includes(p.id) && connection.profile_created_at[p.id] === p.createdAt);
                const unavailableProfiles = connection.profile_ids.length !== granted.length;
                const blocked = disabled || !!connection.revoked_at || connection.rotating;
                return <article key={connection.id} aria-label={connection.name} className="space-y-3 rounded-lg border border-border p-4">
                  <div className="flex flex-wrap justify-between gap-2">
                    <h4 className="font-semibold break-words">{connection.name}</h4>
                    <span className="text-xs text-muted-foreground">{t(connection.revoked_at ? "mcp.revoked" : connection.rotating ? "mcp.rotationBlocked" : "mcp.authorized")}</span>
                  </div>
                  <div className="flex flex-wrap gap-2">{granted.map(p => <span key={p.id} className="rounded bg-muted px-2 py-1 text-xs">{p.name}</span>)}</div>
                  {unavailableProfiles && <p className="text-xs text-vt-warn">{t("mcp.profileUnavailable")}</p>}
                  <ul className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">{connection.scopes.map(scope => <li key={scope}>{scopeLabel(scope)}</li>)}</ul>
                  <p className="text-xs text-muted-foreground">{t("mcp.lastActivity")}: {connection.last_activity_at ? date(connection.last_activity_at) : t("mcp.never")}</p>
                  <div className="flex flex-wrap gap-2">
                    <Button size="sm" variant="outline" disabled={blocked || !status.enabled} onClick={() => {
                      setMessage(null);
                      void mcp.execute(() => mcpClient.copyClientConfig(connection.id), false).then(result => {
                        if (result.ok) { setConfigPreview(JSON.stringify(result.value, null, 2)); setMessage(t("mcp.copied")); }
                      });
                    }}>{t("mcp.copy")}</Button>
                    <Button size="sm" variant="outline" disabled={blocked || !status.enabled || !status.runtime_running} onClick={() => { void act(() => mcpClient.testConnection(connection.id), t("mcp.tested")); }}>{t("mcp.test")}</Button>
                    <Button size="sm" variant="ghost" disabled={disabled || !!connection.revoked_at} onClick={() => { void act(() => mcpClient.rotateConnection(connection.id), t("mcp.rotated")); }}>{t(connection.rotating ? "mcp.retryRotation" : "mcp.rotate")}</Button>
                    <Button size="sm" variant="ghost" disabled={disabled || !!connection.revoked_at} onClick={() => { void act(() => mcpClient.revokeConnection(connection.id)); }}>{t("mcp.revoke")}</Button>
                  </div>
                </article>;
              })}
            </section>
            <div className="space-y-2 text-xs text-muted-foreground">
              <p>{t("mcp.importHelp")}</p>
              {configPreview && <pre aria-label={t("mcp.configExample")} className="max-h-60 overflow-auto rounded-md border border-border bg-muted p-3 text-[11px]">{configPreview}</pre>}
            </div>
            <section className="space-y-3 border-t border-border pt-5">
              <div className="flex items-center justify-between gap-3">
                <h3 className="font-semibold">{t("mcp.activity")}</h3>
                <Button variant="ghost" size="sm" disabled={disabled} onClick={() => { setMessage(null); void mcp.refreshAudit(); }}>{t("mcp.refreshActivity")}</Button>
              </div>
              <p className="text-xs text-muted-foreground">{t("mcp.auditHelp")}</p>
              {mcp.audit && (mcp.audit.length ? <div className="max-h-80 overflow-auto"><table aria-label={t("mcp.activity")} className="w-full text-left text-xs">
                <thead><tr>{["time", "connection", "profile", "action", "result"].map(key => <th key={key} className="p-2 font-medium">{t(`mcp.columns.${key}`)}</th>)}</tr></thead>
                <tbody>{[...mcp.audit].reverse().map((entry, index) => <tr key={`${entry.timestamp}-${index}`} className="border-t border-border">
                  <td className="p-2 whitespace-nowrap">{date(entry.timestamp)}</td>
                  <td className="p-2">{status?.connections.find(c => c.id === entry.connection_id)?.name ?? entry.connection_id}</td>
                  <td className="p-2">{profiles.find(p => p.id === entry.profile_id)?.name ?? entry.profile_id ?? "—"}</td>
                  <td className="p-2">{entry.tool}</td><td className="p-2">{entry.result}</td>
                </tr>)}</tbody>
              </table></div> : <p className="text-muted-foreground">{t("mcp.noActivity")}</p>)}
            </section>
          </>}
          <details className="border-t border-border pt-4 text-xs text-muted-foreground">
            <summary className="cursor-pointer font-medium text-foreground">{t("mcp.newPc")}</summary>
            <p className="mt-3 max-w-prose">{t("mcp.newPcHelp")}</p>
            <p className="mt-2 max-w-prose">{t("mcp.backupHelp")}</p>
          </details>
        </div>
      </section>
      <Dialog open={resetOpen} onOpenChange={open => { if (!mcp.busy) setResetOpen(open); }}>
        <DialogContent><DialogHeader><DialogTitle>{t("mcp.resetTitle")}</DialogTitle><DialogDescription>{t("mcp.resetHelp")}</DialogDescription></DialogHeader>
          {mcp.error && <p role="alert" className="text-sm text-destructive">{mcp.error}</p>}
          <DialogFooter>
            <Button variant="outline" disabled={mcp.busy} onClick={() => setResetOpen(false)}>{t("common.cancel")}</Button>
            <Button variant="destructive" disabled={mcp.busy} onClick={() => { void act(mcpClient.resetInstallation, t("mcp.resetDone")).then(ok => { if (ok) { setResetOpen(false); setConfigPreview(null); } }); }}>{t("mcp.confirmReset")}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
