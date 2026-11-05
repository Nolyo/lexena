//! Official rmcp lifecycle/framing; authenticated IPC supplies immutable actors.
use super::{
    audit::{AuditEntry, AuditLog, AuditResult, AuditTool},
    connections::{GrantStore, Scope, SessionActor},
    contract,
    error::BusinessError,
    tools::{self, EditorAdapter, MutationWork, SessionData},
};
use parking_lot::Mutex;
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ErrorData, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
    RoleServer, ServerHandler, ServiceExt,
};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::{Notify, Semaphore};

#[derive(Default)]
struct Jobs {
    state: Mutex<(bool, usize)>,
    changed: Notify,
}
struct Job(Arc<Jobs>);
impl Drop for Job {
    fn drop(&mut self) {
        self.0.state.lock().1 -= 1;
        self.0.changed.notify_waiters();
    }
}
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
struct AuditAttempt {
    record: Arc<AuditRecord>,
}
struct AuditRecord {
    server: Arc<McpServer>,
    state: Mutex<AuditState>,
}
struct AuditState {
    entry: AuditEntry,
    started: Instant,
    recorded: bool,
}
impl AuditRecord {
    fn result(&self, result: &Result<Value, BusinessError>, authoritative: bool) {
        let mut state = self.state.lock();
        if state.recorded && !authoritative {
            return;
        }
        state.entry.result = audit_result(result);
        if let Ok(value) = result {
            if let Some(id) = value.get("entity_id").and_then(Value::as_str) {
                state.entry.object_id = Some(id.into());
            }
        }
        if authoritative {
            state.entry.timestamp = chrono::Utc::now();
            state.entry.duration_ms =
                state.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let _ = self.server.audit.append(state.entry.clone());
            state.recorded = true;
        }
    }
}
impl Drop for AuditAttempt {
    fn drop(&mut self) {
        let mut state = self.record.state.lock();
        if !state.recorded {
            state.entry.duration_ms =
                state.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            let _ = self.record.server.audit.append(state.entry.clone());
            state.recorded = true;
        }
    }
}
pub struct McpServer {
    app_data: PathBuf,
    grants: Arc<GrantStore>,
    editors: Arc<dyn EditorAdapter>,
    audit: AuditLog,
    jobs: Arc<Jobs>,
    limits: Mutex<HashMap<uuid::Uuid, Weak<Semaphore>>>,
    read_cache: super::read_cache::ReadCache,
    #[cfg(test)]
    job_observer: Mutex<Option<Arc<dyn Fn(&str) + Send + Sync>>>,
}
impl McpServer {
    pub fn new(
        app_data: PathBuf,
        grants: Arc<GrantStore>,
        editors: Arc<dyn EditorAdapter>,
    ) -> Arc<Self> {
        Arc::new(Self {
            audit: AuditLog::new(&app_data),
            app_data,
            grants,
            editors,
            jobs: Arc::new(Jobs::default()),
            limits: Mutex::new(HashMap::new()),
            read_cache: Default::default(),
            #[cfg(test)]
            job_observer: Default::default(),
        })
    }
    pub fn session(self: &Arc<Self>, actor: SessionActor) -> McpSession {
        let mut limits = self.limits.lock();
        limits.retain(|_, v| v.strong_count() > 0);
        let key = actor.connection_id();
        let permits = limits.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
            let p = Arc::new(Semaphore::new(4));
            limits.insert(key, Arc::downgrade(&p));
            p
        });
        McpSession {
            server: self.clone(),
            actor,
            data: Arc::new(SessionData::default()),
            permits,
        }
    }
    pub fn session_handler(self: &Arc<Self>) -> super::runtime::SessionHandler {
        let server = self.clone();
        Arc::new(move |stream, actor| {
            let session = server.session(actor);
            Box::pin(async move {
                let (read, write) = tokio::io::split(stream);
                let transport = rmcp::transport::async_rw::AsyncRwTransport::new(read, write)
                    .with_max_line_length(8_388_608);
                if let Ok(service) = session.serve(transport).await {
                    let _ = service.waiting().await;
                }
            })
        })
    }
    /// Deny new work, then wait for every started synchronous disk job, including
    /// jobs whose caller timed out/disconnected. Call before updater installation.
    pub async fn shutdown(&self) {
        self.jobs.state.lock().0 = true;
        loop {
            let notified = self.jobs.changed.notified();
            if self.jobs.state.lock().1 == 0 {
                break;
            }
            notified.await;
        }
    }
    pub fn is_maintenance(&self) -> bool {
        self.jobs.state.lock().0
    }
    #[cfg(test)]
    pub(super) fn observe_cache(&self, callback: Arc<dyn Fn(&str) + Send + Sync>) {
        self.read_cache.observe(callback);
    }
    #[cfg(test)]
    pub(super) fn observe_jobs(&self, callback: Arc<dyn Fn(&str) + Send + Sync>) {
        *self.job_observer.lock() = Some(callback);
    }
    pub fn resume_after_maintenance(&self) -> Result<(), BusinessError> {
        let mut state = self.jobs.state.lock();
        if state.1 != 0 {
            return Err(BusinessError::new("BUSY"));
        }
        state.0 = false;
        Ok(())
    }
    async fn blocking<T: Send + 'static>(
        &self,
        _stage: &'static str,
        work: impl FnOnce() -> Result<T, BusinessError> + Send + 'static,
    ) -> Result<T, BusinessError> {
        let guard = {
            let mut state = self.jobs.state.lock();
            if state.0 {
                return Err(BusinessError::new("MAINTENANCE"));
            }
            state.1 += 1;
            Job(self.jobs.clone())
        };
        #[cfg(test)]
        let observer = self.job_observer.lock().clone();
        tokio::task::spawn_blocking(move || {
            let _job = guard;
            #[cfg(test)]
            if let Some(observer) = &observer {
                observer(_stage);
            }
            let result = work();
            #[cfg(test)]
            if _stage == "commit" {
                if let Some(observer) = &observer {
                    observer("commit_completed");
                }
            }
            result
        })
        .await
        .map_err(|_| BusinessError::new("INTERNAL_ERROR"))?
    }
}
#[derive(Clone)]
pub struct McpSession {
    server: Arc<McpServer>,
    actor: SessionActor,
    data: Arc<SessionData>,
    permits: Arc<Semaphore>,
}
impl McpSession {
    #[cfg(test)]
    pub(super) fn expire_for_test(&self) {
        self.data.expire_for_test();
    }
    pub async fn invoke(&self, name: &str, value: Value) -> Result<CallToolResult, ErrorData> {
        // Validate required profile before manifest, repository, permission or audit I/O.
        let args = contract::validate(name, value)?;
        // Account for the whole business/audit lifetime, including awaits
        // between disk jobs. This guard drops after AuditAttempt, before SDK
        // response delivery; detached synchronous workers retain their own Job.
        // Once shutdown closes admission, even a denied request must do no I/O.
        let _request = {
            let mut state = self.server.jobs.state.lock();
            if state.0 {
                return Ok(BusinessError::new("MAINTENANCE").result());
            }
            state.1 += 1;
            Job(self.server.jobs.clone())
        };
        let started = Instant::now();
        let audit = Arc::new(AuditRecord {
            server: self.server.clone(),
            state: Mutex::new(AuditState {
                entry: audit_entry(self.actor, name, &args, AuditResult::Cancelled, 0),
                started,
                recorded: false,
            }),
        });
        let _attempt = AuditAttempt {
            record: audit.clone(),
        };
        let result = if self.server.is_maintenance() {
            Err(BusinessError::new("MAINTENANCE"))
        } else {
            match self.permits.clone().try_acquire_owned() {
                Err(_) => Err(BusinessError::new("BUSY")),
                Ok(permit) => {
                    let cancelled = Arc::new(AtomicBool::new(false));
                    let _cancel = CancelOnDrop(cancelled.clone());
                    match tokio::time::timeout(
                        Duration::from_secs(30),
                        self.execute(
                            name,
                            args.clone(),
                            cancelled,
                            Arc::new(permit),
                            audit.clone(),
                        ),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(BusinessError::new("TIMEOUT")),
                    }
                }
            }
        };
        audit.result(&result, false);
        Ok(match result {
            Ok(value) => CallToolResult::structured(value),
            Err(e) => e.result(),
        })
    }
    async fn execute(
        &self,
        name: &str,
        args: contract::Args,
        cancelled: Arc<AtomicBool>,
        permit: Arc<tokio::sync::OwnedSemaphorePermit>,
        audit: Arc<AuditRecord>,
    ) -> Result<Value, BusinessError> {
        let server = self.server.clone();
        let actor = self.actor;
        let tool = name.to_string();
        if contract::permission(name).is_none() {
            return self
                .server
                .blocking("metadata", move || {
                    let _permit = permit;
                    tools::metadata(&server.app_data, &server.grants, actor, &tool)
                })
                .await;
        }
        let a = args.clone();
        let required = contract::permission(name).unwrap();
        let resolving_permit = permit.clone();
        let (scope, profile_name) = self
            .server
            .blocking("resolve", move || {
                let _permit = resolving_permit;
                tools::resolve(&server.app_data, &server.grants, actor, &a, required)
            })
            .await?;
        if required == Scope::NotesRead {
            let server = self.server.clone();
            let data = self.data.clone();
            return self
                .server
                .blocking("read", move || {
                    let _permit = permit;
                    tools::read(
                        &scope,
                        &profile_name,
                        &server.grants,
                        actor,
                        &data,
                        &server.read_cache,
                        &tool,
                        &args,
                    )
                })
                .await;
        }
        let work = Arc::new(MutationWork {
            scope,
            profile_name,
            grants: self.server.grants.clone(),
            actor,
            data: self.data.clone(),
            adapter: self.server.editors.clone(),
            name: tool,
            args,
            cancelled,
        });
        let replay = work.clone();
        let replaying_permit = permit.clone();
        if let Some(receipt) = self
            .server
            .blocking("replay", move || {
                let _permit = replaying_permit;
                replay.replay()
            })
            .await?
        {
            return Ok(receipt);
        }
        let (prepared, collection) = work.prepare().await?;
        self.server
            .blocking("commit", move || {
                let _permit = permit;
                let result = work.commit(prepared, collection);
                // Shared handoff closes the completion/delivery cancellation
                // window. A prior cancelled attempt may precede this outcome;
                // ordinary successful calls are recorded exactly once.
                audit.result(&result, true);
                result
            })
            .await
    }
    pub fn visible_tools(&self) -> Result<Vec<Tool>, ErrorData> {
        self.server
            .grants
            .with_session_authorization(&self.actor, |lease| {
                contract::catalog()
                    .into_iter()
                    .filter(|tool| {
                        contract::permission(&tool.name).is_none_or(|scope| {
                            lease.allows(scope)
                                && (scope == Scope::NotesRead || lease.allows(Scope::NotesRead))
                        })
                    })
                    .collect()
            })
            .map_err(|_| ErrorData::invalid_request("Connection authorization expired.", None))
    }
}
fn audit_result(result: &Result<Value, BusinessError>) -> AuditResult {
    match result {
        Ok(_) => AuditResult::Ok,
        Err(e) if matches!(e.code.as_str(), "NOT_FOUND" | "PERMISSION_DENIED") => {
            AuditResult::Denied
        }
        Err(e)
            if matches!(
                e.code.as_str(),
                "REVISION_CONFLICT" | "IDEMPOTENCY_CONFLICT" | "EDITOR_BUSY"
            ) =>
        {
            AuditResult::Conflict
        }
        Err(e) if e.code == "TIMEOUT" => AuditResult::Cancelled,
        Err(_) => AuditResult::Failed,
    }
}
fn audit_entry(
    actor: SessionActor,
    name: &str,
    args: &contract::Args,
    result: AuditResult,
    duration_ms: u64,
) -> AuditEntry {
    AuditEntry {
        timestamp: chrono::Utc::now(),
        connection_id: actor.connection_id(),
        tool: audit_tool(name),
        profile_id: args.profile_id.clone(),
        object_id: if name.starts_with("lexena_create_") {
            None
        } else {
            args.note_id.clone().or_else(|| args.folder_id.clone())
        },
        result,
        duration_ms,
        operation_id: args
            .operation_id
            .as_ref()
            .and_then(|v| uuid::Uuid::parse_str(v).ok()),
    }
}
impl ServerHandler for McpSession {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(contract::INSTRUCTIONS)
    }
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![
            ProtocolVersion::V_2026_07_28,
            ProtocolVersion::V_2025_11_25,
        ])
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|r| r.cursor.is_some()) {
            return Err(super::error::invalid_params("CURSOR_INVALID"));
        }
        let mut result = ListToolsResult::default();
        result.tools = self.visible_tools()?;
        Ok(result)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // get_tool deliberately uses the SDK default: our typed validation must
        // preserve PROFILE_REQUIRED (the SDK generic validator has no such code).
        let future = self.invoke(
            &request.name,
            Value::Object(request.arguments.unwrap_or_default()),
        );
        tokio::select! {result=future=>result.map(Into::into),_=context.ct.cancelled()=>Ok(BusinessError::new("TIMEOUT").result().into())}
    }
}
fn audit_tool(name: &str) -> AuditTool {
    match name {
        "lexena_get_context" => AuditTool::GetContext,
        "lexena_list_profiles" => AuditTool::ListProfiles,
        "lexena_list_folders" => AuditTool::ListFolders,
        "lexena_list_notes" => AuditTool::ListNotes,
        "lexena_search_notes" => AuditTool::SearchNotes,
        "lexena_get_note" => AuditTool::GetNote,
        "lexena_create_note" => AuditTool::CreateNote,
        "lexena_append_to_note" => AuditTool::AppendNote,
        "lexena_edit_note" => AuditTool::EditNote,
        "lexena_set_note_metadata" => AuditTool::MoveNote,
        "lexena_replace_note_content" => AuditTool::ReplaceNote,
        "lexena_delete_note" => AuditTool::DeleteNote,
        "lexena_restore_note" => AuditTool::RestoreNote,
        "lexena_list_note_versions" => AuditTool::ListHistory,
        "lexena_create_folder" => AuditTool::CreateFolder,
        "lexena_update_folder" => AuditTool::RenameFolder,
        "lexena_delete_folder" => AuditTool::DeleteFolder,
        _ => unreachable!(),
    }
}
