use crate::mcp::connections;
use lexena_mcp_ipc::{EndpointDescriptor, Error, Result};
use std::{future::Future, path::Path, pin::Pin, sync::Arc};
use tokio::{
    sync::{watch, Semaphore},
    task::{JoinHandle, JoinSet},
};
#[cfg(test)]
use uuid::Uuid;

#[cfg(windows)]
pub type SessionHandler = Arc<
    dyn Fn(
            tokio::net::windows::named_pipe::NamedPipeServer,
            connections::SessionActor,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Owns the pipe listener and session tasks. Dropping or stopping closes all
/// streams; the service must shield commits already past its durable boundary.
#[cfg(windows)]
pub struct LocalMcpRuntime {
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}
#[cfg(windows)]
impl LocalMcpRuntime {
    pub fn start(
        app_data: &Path,
        store: Arc<connections::GrantStore>,
        handler: SessionHandler,
    ) -> Result<Self> {
        let config = store.snapshot()?;
        if !config.enabled {
            return Err(Error::Denied);
        }
        let descriptor = EndpointDescriptor::new(
            config.installation_id,
            config.server_path,
            env!("CARGO_PKG_VERSION"),
        );
        let endpoint = app_data.join("mcp/endpoint.json");
        // First-instance reservation happens before descriptor publication.
        let mut listener = lexena_mcp_ipc::windows::create_pipe(&descriptor.pipe_name, true)?;
        lexena_mcp_ipc::write_private_json(&endpoint, &descriptor)?;
        let (stop, mut stopped) = watch::channel(false);
        let task = tokio::spawn(async move {
            let authenticator = Arc::new(lexena_mcp_ipc::IpcAuthenticator::new(
                config.installation_id,
            ));
            let semaphore = Arc::new(Semaphore::new(lexena_mcp_ipc::MAX_CONNECTIONS));
            let mut sessions = JoinSet::new();
            let mut changes = store.subscribe();
            let result:Result<()>=async {
                loop {
                    tokio::select! {
                        biased;
                        _=stopped.changed()=>break,
                        _=changes.changed()=> { if !store.snapshot()?.enabled { break; } },
                        Some(_)=sessions.join_next(),if !sessions.is_empty()=>{},
                        connected=listener.connect()=> {
                            connected?;
                            let permit=semaphore.clone().try_acquire_owned();
                            if permit.is_err() {
                                // Disconnect the ninth client before creating a replacement.
                                listener.disconnect()?;
                                continue;
                            }
                            let next=lexena_mcp_ipc::windows::create_pipe(&descriptor.pipe_name,false)?;
                            let mut stream=std::mem::replace(&mut listener,next);
                            let store=store.clone(); let auth=authenticator.clone();let handler=handler.clone();
                            let mut cancelled=stopped.clone();let mut grants=store.subscribe();
                            sessions.spawn(async move {
                                let _permit=permit.unwrap();
                                let authenticated_generation=std::sync::Mutex::new(None);
                                let authenticated=tokio::select! {
                                    result=auth.accept(&mut stream,|id| {
                                        let grant=store.grant(id)?;
                                        let mut generation=authenticated_generation.lock().map_err(|_|Error::Denied)?;
                                        if generation.is_some_and(|previous|previous!=grant.generation) { return Err(Error::Denied); }
                                        *generation=Some(grant.generation);store.secret(id)
                                    })=>result,
                                    _=cancelled.changed()=>return,
                                };
                                let Ok(actor)=authenticated else { return; };
                                let Ok(grant)=store.grant(actor) else { return; };
                                if authenticated_generation.lock().ok().and_then(|g|*g)!=Some(grant.generation) { return; }
                                let generation=grant.generation;
                                if store.record_activity(actor).is_err() { return; }
                                let session_actor=connections::SessionActor::authenticated(config.installation_id,actor,generation);
                                let service=handler(stream,session_actor);tokio::pin!(service);
                                loop { tokio::select! {
                                    _=&mut service=>break,
                                    _=cancelled.changed()=>break,
                                    _=grants.changed()=> { if !store.grant(actor).is_ok_and(|g|g.generation==generation) { break; } }
                                } }
                            });
                        }
                    }
                } Ok(())
            }.await;
            drop(listener);
            sessions.abort_all();
            while sessions.join_next().await.is_some() {}
            // Never remove a descriptor published by a replacement installation.
            if EndpointDescriptor::read(&endpoint).is_ok_and(|d| {
                d.installation_id == descriptor.installation_id
                    && d.server_path == descriptor.server_path
            }) {
                let _ = std::fs::remove_file(endpoint);
            }
            result
        });
        Ok(Self {
            stop,
            task: Some(task),
        })
    }
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
    pub async fn stop(mut self) -> Result<()> {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await.map_err(|_| Error::Denied)??;
        }
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for LocalMcpRuntime {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    #[tokio::test]
    async fn authenticated_actor_generation_is_rejected_before_runtime_can_observe_rotation() {
        use tokio::io::AsyncReadExt;
        use tokio::net::windows::named_pipe::ClientOptions;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(connections::GrantStore::memory());
        let grant = store
            .create(
                "session".into(),
                vec![super::super::permissions::ProfileIdentity {
                    id: "default".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                }],
                vec![connections::Scope::NotesWrite],
            )
            .unwrap();
        let installation = store.snapshot().unwrap().installation_id;
        struct Cleanup(Uuid, Uuid);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = lexena_mcp_ipc::delete_secret(self.0, self.1);
            }
        }
        let _cleanup = Cleanup(installation, grant.id);
        let (entered, mut actors) = tokio::sync::mpsc::unbounded_channel();
        let handler: SessionHandler = Arc::new(move |mut pipe, actor| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.send(actor).unwrap();
                let mut byte = [0; 1];
                let _ = pipe.read(&mut byte).await;
            })
        });
        let runtime = LocalMcpRuntime::start(dir.path(), store.clone(), handler).unwrap();
        let mut client = ClientOptions::new()
            .open(&format!(r"\\.\pipe\lexena-mcp-{installation}"))
            .unwrap();
        lexena_mcp_ipc::authenticate_client(
            &mut client,
            installation,
            grant.id,
            &store.secret(grant.id).unwrap(),
        )
        .await
        .unwrap();
        let actor = tokio::time::timeout(std::time::Duration::from_secs(2), actors.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actor.installation_id(), installation);
        assert_eq!(actor.connection_id(), grant.id);
        assert_eq!(actor.generation(), grant.generation);
        store.rotate(grant.id).unwrap();
        // Current-thread Tokio: no await between rotation and this call, so the
        // watch-driven cancellation task has not had a chance to observe it.
        let mut committed = false;
        assert!(store
            .with_authorization(
                &actor,
                "default",
                "2026-01-01T00:00:00Z",
                &[connections::Scope::NotesWrite],
                |_| {
                    committed = true;
                }
            )
            .is_err());
        assert!(!committed);
        runtime.stop().await.unwrap();
    }
    #[tokio::test]
    async fn stopped_runtime_closes_authenticated_sessions_and_allows_restart() {
        use tokio::io::AsyncReadExt;
        use tokio::net::windows::named_pipe::ClientOptions;
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(connections::GrantStore::memory());
        let config = store.snapshot().unwrap();
        let handler: SessionHandler = std::sync::Arc::new(|mut pipe, _| {
            Box::pin(async move {
                let mut b = [0; 1];
                let _ = pipe.read_exact(&mut b).await;
            })
        });
        let runtime = LocalMcpRuntime::start(dir.path(), store.clone(), handler.clone()).unwrap();
        let mut client = ClientOptions::new()
            .open(&format!(r"\\.\pipe\lexena-mcp-{}", config.installation_id))
            .unwrap();
        runtime.stop().await.unwrap();
        let mut byte = [0; 1];
        assert!(
            client.read(&mut byte).await.is_err() || client.read(&mut byte).await.unwrap_or(0) == 0
        );
        let runtime = LocalMcpRuntime::start(dir.path(), store, handler).unwrap();
        runtime.stop().await.unwrap();
    }
    #[tokio::test]
    async fn eight_live_authenticated_pipes_ninth_denied_and_revocation_closes_all() {
        use tokio::io::AsyncReadExt;
        use tokio::net::windows::named_pipe::ClientOptions;
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(connections::GrantStore::memory());
        let grant = store
            .create(
                "test".into(),
                vec![super::super::permissions::ProfileIdentity {
                    id: "default".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                }],
                vec![connections::Scope::NotesRead],
            )
            .unwrap();
        let installation = store.snapshot().unwrap().installation_id;
        struct Cleanup(uuid::Uuid, uuid::Uuid);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = lexena_mcp_ipc::delete_secret(self.0, self.1);
            }
        }
        let _cleanup = Cleanup(installation, grant.id);
        let secret = lexena_mcp_ipc::read_secret(installation, grant.id).unwrap();
        let (entered, mut observed) = tokio::sync::mpsc::unbounded_channel();
        let handler: SessionHandler = std::sync::Arc::new(move |mut pipe, _| {
            let entered = entered.clone();
            Box::pin(async move {
                entered.send(()).unwrap();
                let mut b = [0; 1];
                let _ = pipe.read_exact(&mut b).await;
            })
        });
        let runtime = LocalMcpRuntime::start(dir.path(), store.clone(), handler).unwrap();
        let name = format!(r"\\.\pipe\lexena-mcp-{installation}");
        let mut clients = Vec::new();
        for _ in 0..8 {
            let mut pipe = ClientOptions::new().open(&name).unwrap();
            lexena_mcp_ipc::authenticate_client(&mut pipe, installation, grant.id, &secret)
                .await
                .unwrap();
            clients.push(pipe);
            tokio::time::timeout(std::time::Duration::from_secs(2), observed.recv())
                .await
                .unwrap()
                .unwrap();
        }
        if let Ok(mut ninth) = ClientOptions::new().open(&name) {
            assert!(lexena_mcp_ipc::authenticate_client(
                &mut ninth,
                installation,
                grant.id,
                &secret
            )
            .await
            .is_err());
        }
        store.revoke(grant.id).unwrap();
        for client in &mut clients {
            let mut b = [0; 1];
            let read = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut b))
                .await
                .unwrap();
            assert!(read.is_err() || read.unwrap() == 0);
        }
        runtime.stop().await.unwrap();
    }
}
