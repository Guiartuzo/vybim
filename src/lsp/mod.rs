//! A generic, language-agnostic LSP client.
//!
//! This is the substrate the language features build on; it ships **no**
//! user-facing feature by itself (the first consumer is the go-to-definition
//! change). Some of the API here is therefore forward-declared for that
//! consumer, hence the module-wide `dead_code` allowance below — it is removed
//! once the feature lands and exercises the request/supersede/position paths.
#![allow(dead_code)]

pub mod client;
pub mod protocol;
pub mod registry;
pub mod transport;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use serde_json::Value;

use crate::app::AppEvent;
use client::{PendingKind, Server, file_uri};
pub use protocol::Message;
use registry::{Registry, language_of};
use transport::Connection;

/// A running server: its protocol state plus the live subprocess connection,
/// and an outgoing queue held until the `initialize` handshake completes (LSP
/// forbids sending document notifications before `initialized`).
#[derive(Debug)]
struct Running {
    server: Server,
    conn: Connection,
    ready: bool,
    queued: Vec<Value>,
}

impl Running {
    /// Send now if the handshake is done, else queue until it is.
    fn send_or_queue(&mut self, body: Value) {
        if self.ready {
            let _ = self.conn.send(&body);
        } else {
            self.queued.push(body);
        }
    }

    fn flush(&mut self) {
        for body in self.queued.drain(..) {
            let _ = self.conn.send(&body);
        }
    }
}

/// A response routed back to the feature that issued the request, for the main
/// loop to act on. (No consumer in this change beyond the internal
/// `initialize` handling; go-to-definition consumes the `Definition` arm.)
#[derive(Debug)]
pub struct Response {
    pub language: String,
    pub kind: PendingKind,
    pub result: Option<Value>,
}

/// The set of running language servers, one per language, lazily started.
#[derive(Debug)]
pub struct Lsp {
    registry: Registry,
    /// Running servers keyed by language.
    servers: HashMap<String, Running>,
    log_dir: PathBuf,
    next_id: usize,
    /// A one-line status set when a server starts or dies, for the app to show.
    pub status: Option<String>,
}

impl Lsp {
    pub fn new() -> Self {
        let log_dir = std::env::temp_dir().join("vybim-lsp");
        let _ = std::fs::create_dir_all(&log_dir);
        Self {
            registry: Registry::load(),
            servers: HashMap::new(),
            log_dir,
            next_id: 0,
            status: None,
        }
    }

    /// Lazily start the server for `language` if one is registered/installed and
    /// not already running. Returns whether a server is now present.
    fn ensure_started(&mut self, language: &str, root: &Path, tx: &Sender<AppEvent>) -> bool {
        if self.servers.contains_key(language) {
            return true;
        }
        let Some(cmd) = self.registry.resolve(language) else {
            return false;
        };
        let id = self.next_id;
        self.next_id += 1;
        let log = std::fs::File::create(self.log_dir.join(format!("{language}.log"))).ok();
        match Connection::spawn(id, &cmd, root, tx.clone(), log) {
            Ok(mut conn) => {
                let mut server = Server::new(id, language);
                // `initialize` is the one message allowed before `initialized`.
                let _ = conn.send(&server.initialize(root));
                self.servers.insert(
                    language.to_string(),
                    Running {
                        server,
                        conn,
                        ready: false,
                        queued: Vec::new(),
                    },
                );
                self.status = Some(format!("LSP: started {} for {language}", cmd.program));
                true
            }
            Err(e) => {
                self.status = Some(format!("LSP: failed to start {}: {e}", cmd.program));
                false
            }
        }
    }

    /// A file was opened: start its server if needed and send `didOpen`.
    pub fn notify_opened(&mut self, path: &Path, text: &str, root: &Path, tx: &Sender<AppEvent>) {
        let Some(language) = language_of(path) else {
            return;
        };
        if !self.ensure_started(language, root, tx) {
            return;
        }
        let uri = file_uri(path);
        let running = self.servers.get_mut(language).expect("just ensured");
        if running.server.is_open(&uri) {
            return;
        }
        let body = running.server.did_open(&uri, language, text);
        running.send_or_queue(body);
    }

    /// A served file's buffer changed: send a (coalesced) full-document `didChange`.
    pub fn notify_changed(&mut self, path: &Path, text: &str) {
        let Some(language) = language_of(path) else {
            return;
        };
        let uri = file_uri(path);
        if let Some(running) = self.servers.get_mut(language)
            && let Some(body) = running.server.did_change(&uri, text)
        {
            running.send_or_queue(body);
        }
    }

    /// A served file closed: send `didClose`.
    pub fn notify_closed(&mut self, path: &Path) {
        let Some(language) = language_of(path) else {
            return;
        };
        let uri = file_uri(path);
        if let Some(running) = self.servers.get_mut(language)
            && let Some(body) = running.server.did_close(&uri)
        {
            running.send_or_queue(body);
        }
    }

    /// How many servers are currently running (used in tests to assert inertness).
    pub fn running_count(&self) -> usize {
        self.servers.len()
    }

    /// A server for `language`, if running — for a feature to issue a request.
    pub fn server_mut(&mut self, language: &str) -> Option<&mut Server> {
        self.servers.get_mut(language).map(|r| &mut r.server)
    }

    /// Send an already-built request/notification body to `language`'s server.
    pub fn send(&mut self, language: &str, body: Value) {
        if let Some(running) = self.servers.get_mut(language) {
            running.send_or_queue(body);
        }
    }

    /// Handle an incoming message. The `initialize` response is consumed here
    /// (store capabilities, send `initialized`, flush the queue); other
    /// responses are routed back to their feature via the returned [`Response`].
    pub fn handle_message(&mut self, server_id: usize, msg: Message) -> Option<Response> {
        let (language, running) = self.by_id_mut(server_id)?;
        match msg {
            Message::Response { id, result, error } => {
                let kind = running.server.on_response(&id)?;
                match kind {
                    PendingKind::Initialize => {
                        let initialized = running.server.on_initialize_response(result.as_ref());
                        let _ = running.conn.send(&initialized);
                        running.ready = true;
                        running.flush();
                        None
                    }
                    other => Some(Response {
                        language,
                        kind: other,
                        // Surface an error result as `None` (no location).
                        result: if error.is_some() { None } else { result },
                    }),
                }
            }
            // v1 stores/ignores server notifications and server-to-client
            // requests (no diagnostics UI, no dynamic registration yet).
            Message::Notification { .. } | Message::Request { .. } => None,
        }
    }

    /// Mark a server dead after `LspExit`, removing it so a later open can
    /// restart it (v1 restart policy: none automatic; next open re-spawns).
    pub fn mark_exited(&mut self, server_id: usize) {
        if let Some((language, _)) = self.by_id_mut(server_id) {
            self.servers.remove(&language);
            self.status = Some(format!("LSP: {language} server exited"));
        }
    }

    /// Cleanly shut down every server (on quit).
    pub fn shutdown_all(&mut self) {
        for running in self.servers.values_mut() {
            let (shutdown, exit) = running.server.shutdown();
            let _ = running.conn.send(&shutdown);
            let _ = running.conn.send(&exit);
        }
        self.servers.clear();
    }

    fn by_id_mut(&mut self, server_id: usize) -> Option<(String, &mut Running)> {
        self.servers
            .iter_mut()
            .find(|(_, r)| r.server.id == server_id)
            .map(|(lang, r)| (lang.clone(), r))
    }
}

impl Default for Lsp {
    fn default() -> Self {
        Self::new()
    }
}
