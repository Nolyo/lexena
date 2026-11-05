//! Included as unit tests by mcp/mod.rs and by the isolated actual-source harness.
use super::{
    connections::{GrantStore, Scope, SessionActor},
    contract,
    server::{McpServer, McpSession},
    tools::LocalEditors,
};
use crate::note_service::{NoteRepository, ProfileScope};
use serde_json::{json, Value};
use std::sync::Arc;

struct Fixture {
    dir: tempfile::TempDir,
    grants: Arc<GrantStore>,
    actor: SessionActor,
    server: Arc<McpServer>,
    session: McpSession,
}
impl Fixture {
    fn new(scopes: Vec<Scope>, profiles: Vec<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("profiles/perso")).unwrap();
        std::fs::create_dir_all(dir.path().join("profiles/work")).unwrap();
        std::fs::write(dir.path().join("profiles.json"),json!({"active":"perso","profiles":[{"id":"perso","name":"Perso","createdAt":"2026-01-01T00:00:00Z"},{"id":"work","name":"Secret Work","createdAt":"2026-01-01T00:00:00Z"}]}).to_string()).unwrap();
        let grants = Arc::new(GrantStore::memory());
        let grant = grants
            .insert_grant(
                "test".into(),
                profiles.into_iter().map(str::to_string).collect(),
                scopes,
            )
            .unwrap();
        let actor = SessionActor::authenticated(
            grants.snapshot().unwrap().installation_id,
            grant.id,
            grant.generation,
        );
        let server = McpServer::new(dir.path().into(), grants.clone(), Arc::new(LocalEditors));
        let session = server.session(actor);
        Self {
            dir,
            grants,
            actor,
            server,
            session,
        }
    }
    async fn call(&self, name: &str, args: Value) -> Value {
        let result = self.session.invoke(name, args).await.unwrap();
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert_eq!(
            serde_json::to_value(&result).unwrap()["content"][0]["text"],
            result.structured_content.as_ref().unwrap().to_string()
        );
        result.structured_content.unwrap()
    }
}
async fn error(session: &McpSession, name: &str, args: Value) -> String {
    let r = session.invoke(name, args).await.unwrap();
    assert_eq!(r.is_error, Some(true));
    r.structured_content.unwrap()["code"]
        .as_str()
        .unwrap()
        .into()
}
async fn create(f: &Fixture, content: &str) -> Value {
    f.call("lexena_create_note",json!({"profile_id":"perso","content":content,"format":"text","operation_id":uuid::Uuid::new_v4()})).await
}
fn all() -> Vec<Scope> {
    vec![
        Scope::NotesRead,
        Scope::NotesWrite,
        Scope::NotesDelete,
        Scope::NotesReplace,
        Scope::NotesLocalOnlyRead,
    ]
}

#[tokio::test]
async fn reset_maintenance_rejects_late_requests_without_recreating_activity_files() {
    let f = Fixture::new(all(), vec!["perso"]);
    create(&f, "erased with reset").await;
    f.server.shutdown().await;
    let root = f.dir.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::note_service::ProfileRegistry::reset_app_data_in(
            &root,
            &crate::note_service::editors::EditorRegistry::default(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(std::fs::read_dir(f.dir.path()).unwrap().count(), 0);
    assert_eq!(
        error(
            &f.session,
            "lexena_list_notes",
            json!({"profile_id":"perso"})
        )
        .await,
        "MAINTENANCE"
    );
    assert_eq!(
        std::fs::read_dir(f.dir.path()).unwrap().count(),
        0,
        "a denied late MCP request recreated activity files after all-data reset"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reset_shutdown_drains_admitted_async_business_and_its_final_audit() {
    use super::tools::{Change, EditorAdapter, PrepareFuture};
    use crate::note_service::editors::EditPermit;
    struct HoldPrepare {
        entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: tokio::sync::Notify,
    }
    impl EditorAdapter for HoldPrepare {
        fn prepare<'a>(&'a self, scope: &'a ProfileScope, note: &'a str) -> PrepareFuture<'a> {
            Box::pin(async move {
                if let Some(tx) = self.entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                self.release.notified().await;
                LocalEditors.prepare(scope, note).await
            })
        }
        fn finish(&self, permit: &EditPermit, committed: bool) {
            LocalEditors.finish(permit, committed);
        }
        fn changed(&self, change: Change) {
            LocalEditors.changed(change);
        }
    }
    let f = Fixture::new(all(), vec!["perso"]);
    let note = create(&f, "before reset").await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let hold = Arc::new(HoldPrepare {
        entered: std::sync::Mutex::new(Some(tx)),
        release: tokio::sync::Notify::new(),
    });
    let server = McpServer::new(f.dir.path().into(), f.grants.clone(), hold.clone());
    let request = server.session(f.actor);
    let running = tokio::spawn(async move {
        request.invoke("lexena_append_to_note", json!({"profile_id":"perso", "note_id":note["entity_id"],
            "content":"late", "format":"text", "expected_revision":note["revision"], "operation_id":uuid::Uuid::new_v4()})).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    let stopping_server = server.clone();
    let mut stopping = tokio::spawn(async move { stopping_server.shutdown().await });
    let waited = tokio::time::timeout(std::time::Duration::from_millis(100), &mut stopping)
        .await
        .is_err();
    hold.release.notify_one();
    running.await.unwrap().unwrap();
    if waited {
        tokio::time::timeout(std::time::Duration::from_secs(5), stopping)
            .await
            .unwrap()
            .unwrap();
    }
    f.server.shutdown().await;
    let root = f.dir.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        crate::note_service::ProfileRegistry::reset_app_data_in(
            &root,
            &crate::note_service::editors::EditorRegistry::default(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert!(
        waited,
        "shutdown returned while admitted business work could still write its final audit"
    );
    assert_eq!(std::fs::read_dir(f.dir.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_after_commit_before_delivery_keeps_authoritative_audit_outcome() {
    let f = Fixture::new(all(), vec!["perso"]);
    let operation = uuid::Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sender = Arc::new(std::sync::Mutex::new(Some(tx)));
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let wait = release.clone();
    f.server.observe_jobs(Arc::new(move |stage| {
        if stage == "commit_completed" {
            if let Some(tx) = sender.lock().unwrap().take() {
                let _ = tx.send(());
                let mut ready = wait.0.lock().unwrap();
                while !*ready {
                    ready = wait.1.wait(ready).unwrap();
                }
            }
        }
    }));
    let session = f.session.clone();
    let running = tokio::spawn(async move {
        session.invoke("lexena_create_note",json!({"profile_id":"perso","content":"Committed before delivery","format":"text","operation_id":operation})).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    running.abort();
    let _ = running.await;
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    f.server.shutdown().await;
    let audit = super::audit::AuditLog::new(f.dir.path()).list().unwrap();
    assert!(
        audit
            .iter()
            .any(|entry| entry.operation_id == Some(operation)
                && matches!(entry.result, super::audit::AuditResult::Ok)),
        "completed durable outcome was lost between worker completion and response delivery"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_replay_workers_keep_connection_slots_until_actual_work_finishes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::new(all(), vec!["perso"]);
    let note = create(&f, "No cancelled append").await;
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let hold = release.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let holder = tokio::task::spawn_blocking(move || {
        scope.transaction(|_| {
            let _ = tx.send(());
            let mut ready = hold.0.lock().unwrap();
            while !*ready {
                ready = hold.1.wait(ready).unwrap();
            }
            Ok(())
        })
    });
    rx.await.unwrap();
    let entered = Arc::new(AtomicUsize::new(0));
    let changed = Arc::new(tokio::sync::Notify::new());
    let count = entered.clone();
    let signal = changed.clone();
    f.server.observe_jobs(Arc::new(move |stage| {
        if stage == "replay" {
            count.fetch_add(1, Ordering::SeqCst);
            signal.notify_waiters();
        }
    }));
    let mut calls = Vec::new();
    for _ in 0..4 {
        let session = f.session.clone();
        let args = json!({"profile_id":"perso","note_id":note["entity_id"],"content":"cancelled","format":"text","expected_revision":note["revision"],"operation_id":uuid::Uuid::new_v4()});
        calls.push(tokio::spawn(async move {
            session.invoke("lexena_append_to_note", args).await
        }));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let wake = changed.notified();
            if entered.load(Ordering::SeqCst) == 4 {
                break;
            }
            wake.await;
        }
    })
    .await
    .unwrap();
    for call in calls {
        call.abort();
        let _ = call.await;
    }
    let fifth = f
        .server
        .session(f.actor)
        .invoke("lexena_get_context", json!({}))
        .await
        .unwrap();
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    holder.await.unwrap().unwrap();
    f.server.shutdown().await;
    assert_eq!(
        fifth.structured_content.unwrap()["code"],
        "BUSY",
        "cancelled replay workers released their slots before leaving the held profile transaction"
    );
}
#[test]
fn seventeen_tools_have_derived_schemas_and_annotations() {
    let tools = contract::catalog();
    assert_eq!(tools.len(), 17);
    for tool in tools {
        let v = serde_json::to_value(tool).unwrap();
        assert!(v["outputSchema"].is_object());
        assert!(v["annotations"]["readOnlyHint"].is_boolean());
        let expected = contract::permission(v["name"].as_str().unwrap())
            .map(|s| serde_json::to_value(s).unwrap())
            .unwrap_or(Value::Null);
        assert_eq!(v["_meta"]["lexena.permission"], expected);
        if !["lexena_get_context", "lexena_list_profiles"].contains(&v["name"].as_str().unwrap()) {
            assert!(v["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("profile_id")));
        }
    }
}
#[test]
fn published_catalog_matches_generated_rust_contract() {
    let published: Value =
        serde_json::from_str(include_str!("../../../docs/mcp/tool-catalog.json")).unwrap();
    assert_eq!(
        published["tools"],
        serde_json::to_value(contract::catalog()).unwrap()
    );
    assert_eq!(published["instructions"], contract::INSTRUCTIONS);
    assert_eq!(
        published["supported_protocol_versions"],
        json!(["2026-07-28", "2025-11-25"])
    );
}
#[test]
fn folder_input_bounds_match_cloud_utf16_contract() {
    for name in ["x".repeat(200), "😀".repeat(100)] {
        assert!(contract::validate(
            "lexena_create_folder",
            json!({"profile_id":"perso","name":name,"operation_id":uuid::Uuid::new_v4()})
        )
        .is_ok());
    }
    for name in ["x".repeat(201), "😀".repeat(100) + "x", "  ".into()] {
        assert!(
            contract::validate(
                "lexena_create_folder",
                json!({"profile_id":"perso","name":name,"operation_id":uuid::Uuid::new_v4()})
            )
            .is_err(),
            "overlong folder name accepted"
        );
        assert!(contract::validate("lexena_update_folder",json!({"profile_id":"perso","folder_id":"legacy-folder","name":name,"expected_revision":"revision","operation_id":uuid::Uuid::new_v4()})).is_err());
    }
    for (icon, allowed) in [("😀".repeat(16), true), ("😀".repeat(16) + "x", false)] {
        assert_eq!(contract::validate("lexena_create_folder",json!({"profile_id":"perso","name":"Name","icon":icon,"operation_id":uuid::Uuid::new_v4()})).is_ok(),allowed);
    }
}

#[tokio::test]
async fn idempotency_distinguishes_omitted_patch_fields_from_explicit_null() {
    let f = Fixture::new(all(), vec!["perso"]);
    let folder=f.call("lexena_create_folder",json!({"profile_id":"perso","name":"Original","icon":"⭐","operation_id":uuid::Uuid::new_v4()})).await;
    let n=f.call("lexena_create_note",json!({"profile_id":"perso","folder_id":folder["entity_id"],"content":"Title","format":"text","operation_id":uuid::Uuid::new_v4()})).await;
    let metadata = json!({"profile_id":"perso","note_id":n["entity_id"],"favorite":true,"expected_revision":n["revision"],"operation_id":uuid::Uuid::new_v4()});
    f.call("lexena_set_note_metadata", metadata.clone()).await;
    let mut changed = metadata;
    changed["folder_id"] = Value::Null;
    assert_eq!(
        error(&f.session, "lexena_set_note_metadata", changed).await,
        "IDEMPOTENCY_CONFLICT"
    );
    let update = json!({"profile_id":"perso","folder_id":folder["entity_id"],"name":"Renamed","expected_revision":folder["revision"],"operation_id":uuid::Uuid::new_v4()});
    f.call("lexena_update_folder", update.clone()).await;
    let mut changed = update;
    changed["icon"] = Value::Null;
    assert_eq!(
        error(&f.session, "lexena_update_folder", changed).await,
        "IDEMPOTENCY_CONFLICT"
    );
    f.server.shutdown().await;
}
#[tokio::test]
async fn missing_profile_is_protocol_error_before_disk_io() {
    let f = Fixture::new(all(), vec!["perso"]);
    std::fs::remove_file(f.dir.path().join("profiles.json")).unwrap();
    for t in contract::catalog()
        .into_iter()
        .filter(|t| !["lexena_get_context", "lexena_list_profiles"].contains(&t.name.as_ref()))
    {
        let error = f
            .server
            .session(f.actor)
            .invoke(&t.name, json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.data.unwrap()["code"], "PROFILE_REQUIRED");
    }
}
#[tokio::test]
async fn context_never_discloses_ungranted_profile() {
    let f = Fixture::new(all(), vec!["perso"]);
    for tool in ["lexena_get_context", "lexena_list_profiles"] {
        let v = f.call(tool, json!({})).await;
        assert!(!v.to_string().contains("Secret Work"));
        assert!(!v.to_string().contains("work"));
    }
    crate::note_service::ProfileRegistry::switch_with_preflight_in(
        f.dir.path(),
        "work",
        |_| Ok(()),
        |_, _| {},
    )
    .unwrap();
    let hidden_active = f.call("lexena_get_context", json!({})).await;
    assert!(hidden_active["active_profile_id"].is_null());
    assert!(!hidden_active.to_string().contains("work"));
    let both = Fixture::new(all(), vec!["perso", "work"]);
    assert!(both
        .call("lexena_list_profiles", json!({}))
        .await
        .to_string()
        .contains("Secret Work"));
}
#[tokio::test]
async fn durable_cycle_and_committed_retry_ignore_expired_read_and_revision() {
    let f = Fixture::new(all(), vec!["perso"]);
    let n=f.call("lexena_create_note",json!({"profile_id":"perso","content":"Été\nBody","format":"text","operation_id":uuid::Uuid::new_v4()})).await;
    let id = n["entity_id"].as_str().unwrap();
    let rev = n["revision"].clone();
    let a = json!({"profile_id":"perso","note_id":id,"content":"Added","format":"text","expected_revision":rev,"operation_id":uuid::Uuid::new_v4()});
    let appended = f.call("lexena_append_to_note", a.clone()).await;
    assert_eq!(appended, f.call("lexena_append_to_note", a).await);
    let read = f
        .call(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id,"format":"html"}),
        )
        .await;
    assert!(read["content"].as_str().unwrap().contains("Added"));
    assert!(read["read_token"].is_string());
    let replaced=f.call("lexena_replace_note_content",json!({"profile_id":"perso","note_id":id,"content":"<h1>New title</h1><p>Text</p>","format":"html","read_token":read["read_token"],"expected_revision":read["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let edited=f.call("lexena_edit_note",json!({"profile_id":"perso","note_id":id,"old_text":"Text","new_text":"Edited","expected_revision":replaced["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let folder = f
        .call(
            "lexena_create_folder",
            json!({"profile_id":"perso","name":"Folder","operation_id":uuid::Uuid::new_v4()}),
        )
        .await;
    let folder=f.call("lexena_update_folder",json!({"profile_id":"perso","folder_id":folder["entity_id"],"name":"Renamed","expected_revision":folder["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let moved=f.call("lexena_set_note_metadata",json!({"profile_id":"perso","note_id":id,"folder_id":folder["entity_id"],"favorite":true,"expected_revision":edited["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    f.call("lexena_delete_folder",json!({"profile_id":"perso","folder_id":folder["entity_id"],"expected_revision":folder["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let root = f
        .call(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id,"format":"text"}),
        )
        .await;
    assert!(root["note"]["folder_id"].is_null());
    assert_ne!(moved["revision"], root["revision"]);
    let deleted=f.call("lexena_delete_note",json!({"profile_id":"perso","note_id":id,"expected_revision":root["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let versions = f
        .call(
            "lexena_list_note_versions",
            json!({"profile_id":"perso","note_id":id}),
        )
        .await;
    assert!(!versions.to_string().contains("<h1>"));
    f.call("lexena_restore_note",json!({"profile_id":"perso","note_id":id,"snapshot_id":deleted["snapshot_id"],"expected_revision":deleted["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    assert_eq!(
        NoteRepository::get(&scope, id).unwrap().meta.title,
        "New title"
    );
    f.grants.revoke(f.actor.connection_id()).unwrap();
    let denied = f
        .server
        .session(f.actor)
        .invoke(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id}),
        )
        .await
        .unwrap();
    assert_eq!(denied.structured_content.unwrap()["code"], "NOT_FOUND");
    f.server.shutdown().await;
}

#[tokio::test]
async fn sequential_html_unicode_cursors_tokens_and_cross_connection_rejection() {
    let f = Fixture::new(all(), vec!["perso"]);
    let text = "É🦀中文".repeat(9000);
    let n = create(&f, &text).await;
    let id = n["entity_id"].as_str().unwrap();
    let first = f
        .call(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id,"format":"html"}),
        )
        .await;
    assert!(first["content"].as_str().unwrap().len() <= 32768);
    assert!(first["read_token"].is_null());
    let replacement = |token: Value| json!({"profile_id":"perso","note_id":id,"content":"<p>Replacement</p>","format":"html","read_token":token,"expected_revision":n["revision"],"operation_id":uuid::Uuid::new_v4()});
    assert_eq!(
        error(
            &f.session,
            "lexena_replace_note_content",
            replacement(json!("missing"))
        )
        .await,
        "READ_TOKEN_INVALID"
    );
    let other = f.server.session(f.actor);
    assert_eq!(
        error(
            &other,
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id,"format":"html","cursor":first["next_cursor"]})
        )
        .await,
        "CURSOR_INVALID"
    );
    let mut full = first["content"].as_str().unwrap().to_string();
    let mut page = first;
    while page["next_cursor"].is_string() {
        page=f.call("lexena_get_note",json!({"profile_id":"perso","note_id":id,"format":"html","cursor":page["next_cursor"]})).await;
        assert!(page["content"].as_str().unwrap().len() <= 32768);
        full.push_str(page["content"].as_str().unwrap());
    }
    assert!(full.contains(&text));
    let token = page["read_token"].clone();
    assert!(token.is_string());
    assert_eq!(
        error(
            &other,
            "lexena_replace_note_content",
            replacement(token.clone())
        )
        .await,
        "READ_TOKEN_INVALID"
    );
    for format in ["text", "markdown"] {
        let read = f
            .call(
                "lexena_get_note",
                json!({"profile_id":"perso","note_id":id,"format":format}),
            )
            .await;
        assert!(read["read_token"].is_null());
    }
    let request = replacement(token);
    let committed = f.call("lexena_replace_note_content", request.clone()).await;
    // A fresh session has no token; a committed operation still replays before tokens/editor/revision.
    let replay = other
        .invoke("lexena_replace_note_content", request)
        .await
        .unwrap();
    assert_eq!(replay.structured_content.unwrap(), committed);
    f.server.shutdown().await;
}

#[tokio::test]
async fn pagination_is_bound_to_filters_revision_and_explicit_profile() {
    let f = Fixture::new(all(), vec!["perso", "work"]);
    for _ in 0..3 {
        create(&f, "Même titre\nÉTÉ aperçu").await;
    }
    let page = f
        .call(
            "lexena_search_notes",
            json!({"profile_id":"perso","query":"e\u{301}te\u{301}","limit":1}),
        )
        .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["items"][0]["excerpt"].as_str().unwrap().len() <= 320);
    let cursor = page["next_cursor"].clone();
    assert!(cursor.is_string());
    assert_eq!(
        error(
            &f.session,
            "lexena_search_notes",
            json!({"profile_id":"work","query":"e\u{301}te\u{301}","limit":1,"cursor":cursor})
        )
        .await,
        "CURSOR_INVALID"
    );
    let page = f
        .call("lexena_list_notes", json!({"profile_id":"perso","limit":1}))
        .await;
    create(&f, "Changed collection").await;
    assert_eq!(
        error(
            &f.session,
            "lexena_list_notes",
            json!({"profile_id":"perso","limit":1,"cursor":page["next_cursor"]})
        )
        .await,
        "CURSOR_EXPIRED"
    );
    let note = f
        .call("lexena_list_notes", json!({"profile_id":"perso","limit":1}))
        .await["items"][0]
        .clone();
    let html = f
        .call(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":note["note_id"],"format":"html","limit":3}),
        )
        .await;
    f.call("lexena_append_to_note",json!({"profile_id":"perso","note_id":note["note_id"],"content":"change","format":"text","expected_revision":note["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    assert_eq!(error(&f.session,"lexena_get_note",json!({"profile_id":"perso","note_id":note["note_id"],"format":"html","limit":3,"cursor":html["next_cursor"]})).await,"CURSOR_EXPIRED");
    f.server.shutdown().await;
}

#[tokio::test]
async fn private_notes_snapshots_hidden_and_direct_hidden_tool_denied() {
    use crate::note_service::{journal::RevisionStore, Actor, MutationIntent, NoteService};
    let f = Fixture::new(all(), vec!["perso"]);
    let n = create(&f, "Sensitive private marker").await;
    let id = n["entity_id"].as_str().unwrap();
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    NoteService::mutate(
        &scope,
        MutationIntent::SetLocalOnly {
            id: id.into(),
            local_only: true,
        },
        Actor::Ui,
    )
    .unwrap();
    let grant = f
        .grants
        .insert_grant(
            "restricted".into(),
            vec!["perso".into()],
            vec![Scope::NotesRead],
        )
        .unwrap();
    let actor = SessionActor::authenticated(f.actor.installation_id(), grant.id, grant.generation);
    let session = f.server.session(actor);
    let read = json!({"profile_id":"perso","note_id":id,"format":"html"});
    assert_eq!(error(&session, "lexena_get_note", read).await, "NOT_FOUND");
    let listed = session
        .invoke("lexena_list_notes", json!({"profile_id":"perso"}))
        .await
        .unwrap();
    assert!(!serde_json::to_string(&listed)
        .unwrap()
        .contains("Sensitive"));
    assert!(!session
        .visible_tools()
        .unwrap()
        .iter()
        .any(|t| t.name == "lexena_create_note"));
    assert_eq!(error(&session,"lexena_create_note",json!({"profile_id":"perso","content":"x","format":"text","operation_id":uuid::Uuid::new_v4()})).await,"NOT_FOUND");
    let deleted=f.call("lexena_delete_note",json!({"profile_id":"perso","note_id":id,"expected_revision":RevisionStore::note(&scope,id).unwrap(),"operation_id":uuid::Uuid::new_v4()})).await;
    NoteService::mutate(
        &scope,
        MutationIntent::PurgeNotes {
            note_ids: vec![id.into()],
        },
        Actor::Remote,
    )
    .unwrap();
    assert_eq!(
        error(
            &session,
            "lexena_list_note_versions",
            json!({"profile_id":"perso","note_id":id})
        )
        .await,
        "NOT_FOUND"
    );
    let code=error(&session,"lexena_restore_note",json!({"profile_id":"perso","note_id":id,"snapshot_id":deleted["snapshot_id"],"expected_revision":RevisionStore::note(&scope,id).unwrap(),"operation_id":uuid::Uuid::new_v4()})).await;
    assert_eq!(code, "NOT_FOUND");
    let audit = std::fs::read_to_string(f.dir.path().join("mcp/audit.json")).unwrap();
    assert!(!audit.contains("Sensitive"));
    assert!(!audit.contains("content"));
    f.server.shutdown().await;
}

#[tokio::test]
async fn committed_retry_precedes_dirty_editor_and_changed_revision() {
    use crate::note_service::{editors::editors, journal::RevisionStore};
    let f = Fixture::new(all(), vec!["perso"]);
    let n = create(&f, "Title").await;
    let id = n["entity_id"].as_str().unwrap();
    let request = json!({"profile_id":"perso","note_id":id,"content":"once","format":"text","expected_revision":n["revision"],"operation_id":uuid::Uuid::new_v4()});
    let committed = f.call("lexena_append_to_note", request.clone()).await;
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    let revision = RevisionStore::note(&scope, id).unwrap();
    let context = editors()
        .register("test-mcp-dirty", "perso", id, "test-mcp-dirty", revision)
        .unwrap();
    editors().begin("test-mcp-dirty", &context).unwrap();
    assert_eq!(f.call("lexena_append_to_note", request).await, committed);
    assert_eq!(error(&f.session,"lexena_append_to_note",json!({"profile_id":"perso","note_id":id,"content":"twice","format":"text","expected_revision":committed["revision"],"operation_id":uuid::Uuid::new_v4()})).await,"EDITOR_BUSY");
    // Remove only this synthetic owner without marking another editor clean.
    editors()
        .saved(
            "test-mcp-dirty",
            &context,
            RevisionStore::note(&scope, id).unwrap(),
        )
        .unwrap();
    editors().end("test-mcp-dirty", &context).unwrap();
    f.server.shutdown().await;
}

#[tokio::test]
async fn sdk_negotiates_both_versions_and_calls_real_disk_tools_without_elicitation() {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    for version in ["2026-07-28", "2025-11-25"] {
        let f = Fixture::new(all(), vec!["perso"]);
        let (client, transport) = tokio::io::duplex(65536);
        let session = f.session.clone();
        let service = tokio::spawn(async move {
            let (r, w) = tokio::io::split(transport);
            let s = session
                .serve(
                    rmcp::transport::async_rw::AsyncRwTransport::new(r, w)
                        .with_max_line_length(8_388_608),
                )
                .await
                .unwrap();
            s.waiting().await.unwrap();
        });
        let (read, mut write) = tokio::io::split(client);
        let mut lines = BufReader::new(read).lines();
        let meta = json!({"io.modelcontextprotocol/protocolVersion":version,"io.modelcontextprotocol/clientInfo":{"name":"test-no-elicitation","version":"0"},"io.modelcontextprotocol/clientCapabilities":{}});
        let request = if version == "2026-07-28" {
            json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":meta}})
        } else {
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":version,"capabilities":{},"clientInfo":{"name":"test-no-elicitation","version":"0"}}})
        };
        write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let response: Value = serde_json::from_str(
            &tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(response["error"].is_null(), "{response}");
        if version == "2026-07-28" {
            assert!(response["result"]["supportedVersions"]
                .as_array()
                .unwrap()
                .contains(&json!(version)));
        } else {
            assert_eq!(response["result"]["protocolVersion"], version);
            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                .await
                .unwrap();
        }
        for (id, method, mut params) in [
            (2, "tools/list", json!({})),
            (
                3,
                "tools/call",
                json!({"name":"lexena_create_note","arguments":{"profile_id":"perso","content":"SDK actual disk note","format":"text","operation_id":uuid::Uuid::new_v4()}}),
            ),
            (
                4,
                "tools/call",
                json!({"name":"lexena_list_notes","arguments":{"profile_id":"perso"}}),
            ),
            (
                5,
                "tools/call",
                json!({"name":"lexena_get_note","arguments":{}}),
            ),
        ] {
            if version == "2026-07-28" {
                params["_meta"] = meta.clone();
            }
            let message = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            write
                .write_all(format!("{message}\n").as_bytes())
                .await
                .unwrap();
            let response: Value = serde_json::from_str(
                &tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(response["id"], id);
            match id {
                2 => assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 17),
                3 => assert!(
                    response["result"]["structuredContent"]["entity_id"].is_string(),
                    "{response}"
                ),
                4 => assert_eq!(
                    response["result"]["structuredContent"]["items"][0]["title"],
                    "SDK actual disk note"
                ),
                5 => assert_eq!(response["error"]["data"]["code"], "PROFILE_REQUIRED"),
                _ => unreachable!(),
            }
        }
        write.shutdown().await.unwrap();
        drop(write);
        drop(lines);
        tokio::time::timeout(std::time::Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap();
        f.server.shutdown().await;
    }
}

#[test]
#[ignore = "Infrastructure child explicitly run by release_bridge_sdk_pipe_disk_roundtrip"]
fn pipe_server_fixture() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let app_data = lexena_mcp_ipc::standard_app_data().unwrap();
            let grants = GrantStore::load(&app_data, &std::env::current_exe().unwrap()).unwrap();
            let server = McpServer::new(app_data.clone(), grants.clone(), Arc::new(LocalEditors));
            let runtime =
                super::runtime::LocalMcpRuntime::start(&app_data, grants, server.session_handler())
                    .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(40), async {
                while !app_data.join("test-stop").exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            server.shutdown().await;
            runtime.stop().await.unwrap();
        });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_bridge_sdk_pipe_disk_roundtrip() {
    use std::{path::PathBuf, process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let temporary = tempfile::Builder::new()
        .prefix("Lexena MCP été 研究 ")
        .tempdir()
        .unwrap();
    let server_path = temporary.path().join("lexena.exe");
    let bridge_path = temporary.path().join("lexena-mcp.exe");
    let shipped = std::env::var_os("LEXENA_MCP_TEST_BRIDGE_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("binaries/lexena-mcp-x86_64-pc-windows-msvc.exe")
        });
    std::fs::copy(std::env::current_exe().unwrap(), &server_path).unwrap();
    std::fs::copy(shipped, &bridge_path).unwrap();
    let app_data = temporary.path().join("com.nolyo.lexena");
    std::fs::create_dir_all(app_data.join("profiles/perso")).unwrap();
    std::fs::write(app_data.join("profiles.json"),json!({"active":"perso","profiles":[{"id":"perso","name":"Synthetic","createdAt":"2026-01-01T00:00:00Z"}]}).to_string()).unwrap();
    let grants = GrantStore::load(&app_data, &server_path).unwrap();
    let mut ids = Vec::new();
    for number in 0..2 {
        ids.push(
            grants
                .create(
                    format!("synthetic {number}"),
                    vec![super::permissions::ProfileIdentity {
                        id: "perso".into(),
                        created_at: "2026-01-01T00:00:00Z".into(),
                    }],
                    all(),
                )
                .unwrap()
                .id,
        );
    }
    grants.set_enabled(true).unwrap();
    struct Secrets {
        installation: uuid::Uuid,
        connections: Vec<uuid::Uuid>,
    }
    impl Drop for Secrets {
        fn drop(&mut self) {
            for id in &self.connections {
                let _ = lexena_mcp_ipc::delete_secret(self.installation, *id);
            }
        }
    }
    let _secrets = Secrets {
        installation: grants.snapshot().unwrap().installation_id,
        connections: ids.clone(),
    };
    let mut server = tokio::process::Command::new(&server_path)
        .args([
            "--exact",
            "mcp::local_mcp::pipe_server_fixture",
            "--ignored",
            "--nocapture",
        ])
        .env("APPDATA", temporary.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut bridges = Vec::new();
    let result: anyhow::Result<()>=async {
        tokio::time::timeout(Duration::from_secs(10),async{while !app_data.join("mcp/endpoint.json").exists(){tokio::time::sleep(Duration::from_millis(10)).await;}}).await?;
        struct Client{input:tokio::process::ChildStdin,output:tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,version:&'static str,sequence:i32}
        impl Client {
            async fn request(&mut self,method:&str,mut params:Value)->anyhow::Result<Value>{
                self.sequence+=1;if self.version=="2026-07-28"{params["_meta"]=json!({"io.modelcontextprotocol/protocolVersion":self.version,"io.modelcontextprotocol/clientInfo":{"name":"stdio-synthetic","version":"0"},"io.modelcontextprotocol/clientCapabilities":{}});}
                let message=json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params});self.input.write_all(format!("{message}\n").as_bytes()).await?;self.input.flush().await?;
                let line=tokio::time::timeout(Duration::from_secs(10),self.output.next_line()).await??.ok_or_else(||anyhow::anyhow!("stdio EOF"))?;let response:Value=serde_json::from_str(&line)?;
                anyhow::ensure!(response["id"]==self.sequence&&response["error"].is_null(),"unexpected protocol response: {response}");Ok(response["result"].clone())
            }
            async fn call(&mut self,name:&str,args:Value)->anyhow::Result<Value>{let value=self.request("tools/call",json!({"name":name,"arguments":args})).await?;anyhow::ensure!(value["isError"]!=true,"business error: {value}");Ok(value["structuredContent"].clone())}
        }
        let mut clients=Vec::new();
        for (id,version) in ids.iter().zip(["2026-07-28","2025-11-25"]) {
            let mut child=tokio::process::Command::new(&bridge_path).args(["--connection",&id.to_string()]).env("APPDATA",temporary.path()).current_dir(std::env::temp_dir()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn()?;
            let client=Client{input:child.stdin.take().unwrap(),output:BufReader::new(child.stdout.take().unwrap()).lines(),version,sequence:0};bridges.push(child);clients.push(client);
        }
        // Both authenticated transports remain open concurrently throughout this test.
        for client in &mut clients {
            if client.version=="2026-07-28" {let discovery=client.request("server/discover",json!({})).await?;anyhow::ensure!(discovery["supportedVersions"].as_array().unwrap().contains(&json!(client.version)));}
            else {let initialized=client.request("initialize",json!({"protocolVersion":client.version,"capabilities":{},"clientInfo":{"name":"stdio-synthetic","version":"0"}})).await?;anyhow::ensure!(initialized["protocolVersion"]==client.version);client.input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await?;}
            anyhow::ensure!(client.request("tools/list",json!({})).await?["tools"].as_array().unwrap().len()==17);
        }
        let note=clients[0].call("lexena_create_note",json!({"profile_id":"perso","content":"Pipe été 🦀","format":"text","operation_id":uuid::Uuid::new_v4()})).await?;
        let append=json!({"profile_id":"perso","note_id":note["entity_id"],"content":"One append","format":"text","expected_revision":note["revision"],"operation_id":uuid::Uuid::new_v4()});
        let appended=clients[0].call("lexena_append_to_note",append.clone()).await?;anyhow::ensure!(clients[0].call("lexena_append_to_note",append).await?==appended);
        let read=clients[1].call("lexena_get_note",json!({"profile_id":"perso","note_id":note["entity_id"],"format":"html"})).await?;anyhow::ensure!(read["content"].as_str().unwrap().matches("One append").count()==1&&read["content"].as_str().unwrap().contains("été 🦀"));
        let scope=ProfileScope::resolve_in(&app_data,"perso")?;anyhow::ensure!(NoteRepository::get(&scope,note["entity_id"].as_str().unwrap())?.content.contains("One append"));
        for mut client in clients {client.input.shutdown().await?;drop(client.input);let _=tokio::time::timeout(Duration::from_secs(3),client.output.next_line()).await;}
        Ok(())
    }.await;
    use tokio::io::AsyncReadExt;
    let mut diagnostics = String::new();
    let mut bridge_clean = true;
    for mut child in bridges {
        match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
            Ok(Ok(status)) => bridge_clean &= status.success(),
            _ => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                bridge_clean = false;
            }
        }
        if let Some(mut stderr) = child.stderr.take() {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            diagnostics.push_str(&text);
        }
    }
    std::fs::write(app_data.join("test-stop"), b"stop").unwrap();
    let server_clean = match tokio::time::timeout(Duration::from_secs(5), server.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = server.kill().await;
            let _ = server.wait().await;
            false
        }
    };
    if let Some(mut stderr) = server.stderr.take() {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        diagnostics.push_str(&text);
    }
    if let Some(mut stdout) = server.stdout.take() {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text).await;
        diagnostics.push_str(&text);
    }
    assert!(
        result.is_ok(),
        "{}; synthetic process diagnostics: {diagnostics}",
        result.unwrap_err()
    );
    assert!(
        bridge_clean && server_clean,
        "all synthetic processes must exit cleanly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_does_not_abandon_a_started_durable_job_and_shutdown_drains_it() {
    use super::tools::{Change, EditorAdapter, PrepareFuture};
    use crate::note_service::editors::EditPermit;
    struct Hold {
        entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: (std::sync::Mutex<bool>, std::sync::Condvar),
    }
    impl EditorAdapter for Hold {
        fn prepare<'a>(&'a self, s: &'a ProfileScope, n: &'a str) -> PrepareFuture<'a> {
            LocalEditors.prepare(s, n)
        }
        fn finish(&self, p: &EditPermit, c: bool) {
            LocalEditors.finish(p, c);
        }
        fn changed(&self, _: Change) {
            if let Some(tx) = self.entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let (lock, ready) = &self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = ready.wait(released).unwrap();
            }
        }
    }
    let f = Fixture::new(all(), vec!["perso"]);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let hold = Arc::new(Hold {
        entered: std::sync::Mutex::new(Some(tx)),
        release: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
    });
    let server = McpServer::new(f.dir.path().into(), f.grants.clone(), hold.clone());
    let session = server.session(f.actor);
    let operation = uuid::Uuid::new_v4();
    let args = json!({"profile_id":"perso","content":"Durably committed","format":"text","operation_id":operation});
    let request = session.clone();
    let original = args.clone();
    let running = tokio::spawn(async move { request.invoke("lexena_create_note", original).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    running.abort();
    let _ = running.await;
    let stop_server = server.clone();
    let stopping = tokio::spawn(async move { stop_server.shutdown().await });
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    assert!(
        !stopping.is_finished(),
        "shutdown returned before started durable job finished"
    );
    assert!(server.resume_after_maintenance().is_err());
    assert_eq!(
        error(&session, "lexena_list_notes", json!({"profile_id":"perso"})).await,
        "MAINTENANCE"
    );
    // Disk is already durable, even though caller vanished during event delivery.
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    assert_eq!(NoteRepository::list(&scope).unwrap().len(), 1);
    *hold.release.0.lock().unwrap() = true;
    hold.release.1.notify_all();
    tokio::time::timeout(std::time::Duration::from_secs(5), stopping)
        .await
        .unwrap()
        .unwrap();
    server.resume_after_maintenance().unwrap();
    let audit = super::audit::AuditLog::new(f.dir.path()).list().unwrap();
    assert!(
        audit
            .iter()
            .any(|event| event.operation_id == Some(operation)
                && matches!(event.result, super::audit::AuditResult::Ok)),
        "durable commit vanished from audit after caller cancellation"
    );
    let retried = session.invoke("lexena_create_note", args).await.unwrap();
    assert_ne!(retried.is_error, Some(true));
    assert_eq!(NoteRepository::list(&scope).unwrap().len(), 1);
    server.shutdown().await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn folder_delete_rechecks_collection_after_preparing_known_editors() {
    use super::tools::{EditorAdapter, PrepareFuture};
    use crate::note_service::{editors::EditPermit, Actor, MutationIntent, NoteService};
    struct Inject {
        folder: String,
        once: std::sync::atomic::AtomicBool,
    }
    impl EditorAdapter for Inject {
        fn prepare<'a>(&'a self, s: &'a ProfileScope, n: &'a str) -> PrepareFuture<'a> {
            Box::pin(async move {
                if self.once.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    NoteService::mutate(
                        s,
                        MutationIntent::CreateNoteWithContent {
                            folder_id: Some(self.folder.clone()),
                            content: "new referenced note".into(),
                            format: "text".into(),
                        },
                        Actor::Ui,
                    )?;
                }
                LocalEditors.prepare(s, n).await
            })
        }
        fn finish(&self, p: &EditPermit, c: bool) {
            LocalEditors.finish(p, c);
        }
    }
    let f = Fixture::new(all(), vec!["perso"]);
    let folder = f
        .call(
            "lexena_create_folder",
            json!({"profile_id":"perso","name":"Race","operation_id":uuid::Uuid::new_v4()}),
        )
        .await;
    f.call("lexena_create_note",json!({"profile_id":"perso","folder_id":folder["entity_id"],"content":"Known note","format":"text","operation_id":uuid::Uuid::new_v4()})).await;
    let server = McpServer::new(
        f.dir.path().into(),
        f.grants.clone(),
        Arc::new(Inject {
            folder: folder["entity_id"].as_str().unwrap().into(),
            once: std::sync::atomic::AtomicBool::new(true),
        }),
    );
    assert_eq!(error(&server.session(f.actor),"lexena_delete_folder",json!({"profile_id":"perso","folder_id":folder["entity_id"],"expected_revision":folder["revision"],"operation_id":uuid::Uuid::new_v4()})).await,"REVISION_CONFLICT");
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    let notes = NoteRepository::list(&scope).unwrap();
    assert_eq!(notes.len(), 2);
    assert!(notes
        .iter()
        .all(|n| n.folder_id.as_deref() == folder["entity_id"].as_str()));
    server.shutdown().await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn expiring_cursors_and_html_reads_never_authorize_replacement() {
    let f = Fixture::new(all(), vec!["perso"]);
    let n = create(&f, "A note with enough content to page").await;
    let id = n["entity_id"].as_str().unwrap();
    let page = f
        .call(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id,"format":"html","limit":10}),
        )
        .await;
    let complete = f
        .call(
            "lexena_get_note",
            json!({"profile_id":"perso","note_id":id,"format":"html"}),
        )
        .await;
    f.session.expire_for_test();
    assert_eq!(error(&f.session,"lexena_get_note",json!({"profile_id":"perso","note_id":id,"format":"html","limit":10,"cursor":page["next_cursor"]})).await,"CURSOR_EXPIRED");
    assert_eq!(error(&f.session,"lexena_replace_note_content",json!({"profile_id":"perso","note_id":id,"content":"<p>overwrite</p>","format":"html","read_token":complete["read_token"],"expected_revision":n["revision"],"operation_id":uuid::Uuid::new_v4()})).await,"READ_TOKEN_INVALID");
    let missing=f.session.invoke("lexena_replace_note_content",json!({"profile_id":"perso","note_id":id,"content":"<p>overwrite</p>","format":"html","expected_revision":n["revision"],"operation_id":uuid::Uuid::new_v4()})).await.unwrap_err();
    assert_eq!(missing.data.unwrap()["code"], "INVALID_ARGUMENTS");
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn four_business_calls_are_bounded_across_sessions_of_one_connection() {
    use super::tools::{Change, EditorAdapter, PrepareFuture};
    use crate::note_service::editors::EditPermit;
    struct Hold {
        count: std::sync::atomic::AtomicUsize,
        changed: tokio::sync::Notify,
        release: (std::sync::Mutex<bool>, std::sync::Condvar),
    }
    impl EditorAdapter for Hold {
        fn prepare<'a>(&'a self, s: &'a ProfileScope, n: &'a str) -> PrepareFuture<'a> {
            LocalEditors.prepare(s, n)
        }
        fn finish(&self, p: &EditPermit, c: bool) {
            LocalEditors.finish(p, c);
        }
        fn changed(&self, _: Change) {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.changed.notify_waiters();
            let mut ready = self.release.0.lock().unwrap();
            while !*ready {
                ready = self.release.1.wait(ready).unwrap();
            }
        }
    }
    let f = Fixture::new(all(), vec!["perso"]);
    let hold = Arc::new(Hold {
        count: std::sync::atomic::AtomicUsize::new(0),
        changed: tokio::sync::Notify::new(),
        release: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
    });
    let server = McpServer::new(f.dir.path().into(), f.grants.clone(), hold.clone());
    let mut running = Vec::new();
    for _ in 0..4 {
        let session = server.session(f.actor);
        running.push(tokio::spawn(async move{session.invoke("lexena_create_note",json!({"profile_id":"perso","content":"Concurrent","format":"text","operation_id":uuid::Uuid::new_v4()})).await}));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let changed = hold.changed.notified();
            if hold.count.load(std::sync::atomic::Ordering::SeqCst) == 4 {
                break;
            }
            changed.await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        error(
            &server.session(f.actor),
            "lexena_list_notes",
            json!({"profile_id":"perso"})
        )
        .await,
        "BUSY"
    );
    *hold.release.0.lock().unwrap() = true;
    hold.release.1.notify_all();
    for task in running {
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_ne!(result.is_error, Some(true));
    }
    server.shutdown().await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn ordinary_request_deadline_cancels_preparation_before_any_commit() {
    use super::tools::{EditorAdapter, PrepareFuture};
    use crate::note_service::editors::EditPermit;
    struct Slow;
    impl EditorAdapter for Slow {
        fn prepare<'a>(&'a self, s: &'a ProfileScope, n: &'a str) -> PrepareFuture<'a> {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_secs(31)).await;
                LocalEditors.prepare(s, n).await
            })
        }
        fn finish(&self, p: &EditPermit, c: bool) {
            LocalEditors.finish(p, c);
        }
    }
    let f = Fixture::new(all(), vec!["perso"]);
    let note = create(&f, "Unchanged before timeout").await;
    let server = McpServer::new(f.dir.path().into(), f.grants.clone(), Arc::new(Slow));
    assert_eq!(error(&server.session(f.actor),"lexena_append_to_note",json!({"profile_id":"perso","note_id":note["entity_id"],"content":"must not commit","format":"text","expected_revision":note["revision"],"operation_id":uuid::Uuid::new_v4()})).await,"TIMEOUT");
    server.shutdown().await;
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    assert!(
        !NoteRepository::get(&scope, note["entity_id"].as_str().unwrap())
            .unwrap()
            .content
            .contains("must not commit")
    );
    f.server.shutdown().await;
}

#[tokio::test]
async fn revocation_during_async_prepare_denies_final_locked_commit() {
    use super::tools::{EditorAdapter, PrepareFuture};
    use crate::note_service::editors::EditPermit;
    struct Revoke {
        store: Arc<GrantStore>,
        id: uuid::Uuid,
    }
    impl EditorAdapter for Revoke {
        fn prepare<'a>(&'a self, s: &'a ProfileScope, n: &'a str) -> PrepareFuture<'a> {
            Box::pin(async move {
                let permit = LocalEditors.prepare(s, n).await?;
                self.store.revoke(self.id).unwrap();
                Ok(permit)
            })
        }
        fn finish(&self, p: &EditPermit, c: bool) {
            LocalEditors.finish(p, c);
        }
    }
    let f = Fixture::new(all(), vec!["perso"]);
    let note = create(&f, "Unchanged before revoke").await;
    let server = McpServer::new(
        f.dir.path().into(),
        f.grants.clone(),
        Arc::new(Revoke {
            store: f.grants.clone(),
            id: f.actor.connection_id(),
        }),
    );
    assert_eq!(error(&server.session(f.actor),"lexena_append_to_note",json!({"profile_id":"perso","note_id":note["entity_id"],"content":"must not commit","format":"text","expected_revision":note["revision"],"operation_id":uuid::Uuid::new_v4()})).await,"NOT_FOUND");
    server.shutdown().await;
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    assert!(
        !NoteRepository::get(&scope, note["entity_id"].as_str().unwrap())
            .unwrap()
            .content
            .contains("must not commit")
    );
    f.server.shutdown().await;
}

#[tokio::test]
async fn read_cache_reuses_unchanged_notes_and_invalidates_changed_and_deleted_notes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let f = Fixture::new(all(), vec!["perso"]);
    let a = create(&f, "First cache body").await;
    let b = create(&f, "Second cache body").await;
    let reads = Arc::new(AtomicUsize::new(0));
    let count = reads.clone();
    f.server.observe_cache(Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    f.call("lexena_list_notes", json!({"profile_id":"perso"}))
        .await;
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    f.call(
        "lexena_search_notes",
        json!({"profile_id":"perso","query":"CACHE"}),
    )
    .await;
    assert_eq!(
        reads.load(Ordering::SeqCst),
        2,
        "warm search reread unchanged bodies"
    );
    let updated=f.call("lexena_append_to_note",json!({"profile_id":"perso","note_id":a["entity_id"],"content":"new unique cache phrase","format":"text","expected_revision":a["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let found = f
        .call(
            "lexena_search_notes",
            json!({"profile_id":"perso","query":"new unique cache phrase"}),
        )
        .await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    assert_eq!(found["items"][0]["revision"], updated["revision"]);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "one changed note must not rescan the profile"
    );
    f.call("lexena_delete_note",json!({"profile_id":"perso","note_id":b["entity_id"],"expected_revision":b["revision"],"operation_id":uuid::Uuid::new_v4()})).await;
    let listed = f
        .call("lexena_list_notes", json!({"profile_id":"perso"}))
        .await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        reads.load(Ordering::SeqCst),
        3,
        "deleted note content must not be read"
    );
    f.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_build_releases_acl_lease_and_reauthorizes_before_returning() {
    use crate::note_service::{Actor, MutationIntent, NoteService};
    use std::sync::atomic::{AtomicBool, Ordering};
    let f = Fixture::new(all(), vec!["perso"]);
    let public = create(&f, "Authorized source").await;
    let private = create(&f, "Private never indexed").await;
    let scope = ProfileScope::resolve_in(f.dir.path(), "perso").unwrap();
    NoteService::mutate(
        &scope,
        MutationIntent::SetLocalOnly {
            id: private["entity_id"].as_str().unwrap().into(),
            local_only: true,
        },
        Actor::Ui,
    )
    .unwrap();
    let grant = f
        .grants
        .insert_grant(
            "public-only cache".into(),
            vec!["perso".into()],
            vec![Scope::NotesRead],
        )
        .unwrap();
    let actor = SessionActor::authenticated(f.actor.installation_id(), grant.id, grant.generation);
    let session = f.server.session(actor);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let pause = release.clone();
    let leaked = Arc::new(AtomicBool::new(false));
    let observed = leaked.clone();
    let private_id = private["entity_id"].as_str().unwrap().to_string();
    let public_id = public["entity_id"].as_str().unwrap().to_string();
    f.server.observe_cache(Arc::new(move |id| {
        if id == private_id {
            observed.store(true, Ordering::SeqCst);
        }
        if id == public_id {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(());
                let mut ready = pause.0.lock().unwrap();
                while !*ready {
                    ready = pause.1.wait(ready).unwrap();
                }
            }
        }
    }));
    let request = tokio::spawn(async move {
        session
            .invoke("lexena_list_notes", json!({"profile_id":"perso"}))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    let grants = f.grants.clone();
    let mut revoke = tokio::task::spawn_blocking(move || grants.revoke(grant.id));
    let revoked = tokio::time::timeout(std::time::Duration::from_secs(1), &mut revoke).await;
    // Release first even if the assertion fails, so no fixture thread is stranded.
    *release.0.lock().unwrap() = true;
    release.1.notify_all();
    let promptly = revoked.is_ok();
    if let Ok(result) = revoked {
        result.unwrap().unwrap();
    } else {
        revoke.await.unwrap().unwrap();
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(promptly, "cold read build retained ACL mutex");
    assert_eq!(result.structured_content.unwrap()["code"], "NOT_FOUND");
    assert!(
        !leaked.load(Ordering::SeqCst),
        "public-only cache read private body"
    );
    f.server.shutdown().await;
}

#[tokio::test]
async fn date_filters_compare_instants_and_skip_only_malformed_legacy_entries() {
    let f = Fixture::new(all(), vec!["perso"]);
    let offset = create(&f, "Offset").await;
    let boundary = create(&f, "Boundary").await;
    let malformed = create(&f, "Legacy malformed").await;
    for (note, date) in [
        (&offset, "2026-11-05T10:00:00Z"),
        (&boundary, "2026-11-05T09:30:00Z"),
        (&malformed, "not-a-date"),
    ] {
        let path = f
            .dir
            .path()
            .join("profiles/perso/notes")
            .join(note["entity_id"].as_str().unwrap())
            .join("note.json");
        let mut meta: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        meta["updatedAt"] = json!(date);
        std::fs::write(path, meta.to_string()).unwrap();
    }
    assert_eq!(
        f.call("lexena_list_notes", json!({"profile_id":"perso"}))
            .await["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    for after in ["2026-11-05T11:30:00+02:00", "2026-11-05T09:30:00Z"] {
        let page = f
            .call(
                "lexena_list_notes",
                json!({"profile_id":"perso","updated_after":after}),
            )
            .await;
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["title"], "Offset");
    }
    let before = f
        .call(
            "lexena_search_notes",
            json!({"profile_id":"perso","query":"","updated_before":"2026-11-05T10:30:00+01:00"}),
        )
        .await;
    assert!(
        before["items"].as_array().unwrap().is_empty(),
        "offset-equivalent boundary must be exclusive"
    );
    f.server.shutdown().await;
}
