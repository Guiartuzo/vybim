//! A generic, language-agnostic LSP client — the substrate the language
//! features build on. The go-to-definition feature is its first consumer,
//! exercising the request/supersede/position paths.

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
/// loop to act on (go-to-definition consumes the `Definition` arm).
#[derive(Debug)]
pub struct Response {
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
            // A language we *would* serve but whose binary is missing gets a
            // status-line hint; a truly unmapped language stays a silent no-op.
            if let Some(cmd) = self.registry.mapped(language) {
                self.status = Some(format!(
                    "LSP: {} not found on PATH — no {language} features",
                    cmd.program
                ));
            }
            return false;
        };
        let id = self.next_id;
        self.next_id += 1;
        let log = std::fs::File::create(self.log_dir.join(format!("{language}.log"))).ok();
        match Connection::spawn(id, &cmd, root, tx.clone(), log) {
            Ok(mut conn) => {
                let mut server = Server::new(id);
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
        let fresh_start = !self.servers.contains_key(language);
        if !self.ensure_started(language, root, tx) {
            return;
        }
        // On the first start of a database-driven server (clangd), warn when
        // no compilation database is discoverable: the server still runs, but
        // cross-file navigation silently degrades to header declarations.
        if fresh_start && needs_compile_db(language) && !compile_db_near(path) {
            self.status = Some(format!(
                "LSP: {language} — no compile_commands.json (cross-file navigation limited)"
            ));
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
    #[cfg(test)]
    pub fn running_count(&self) -> usize {
        self.servers.len()
    }

    /// Install a ready, capture-backed server for `language` with the given
    /// capabilities, bypassing a real subprocess. Returns its id and the sink
    /// that captures everything the client sends it.
    #[cfg(test)]
    pub fn install_test_server(
        &mut self,
        language: &str,
        capabilities: Value,
    ) -> (usize, transport::TestSink) {
        let id = self.next_id;
        self.next_id += 1;
        let sink = transport::TestSink::default();
        let mut server = Server::new(id);
        server.capabilities = Some(capabilities);
        self.servers.insert(
            language.to_string(),
            Running {
                server,
                conn: Connection::for_test(sink.clone()),
                ready: true,
                queued: Vec::new(),
            },
        );
        (id, sink)
    }

    /// Decode every message the client wrote to a test sink.
    #[cfg(test)]
    pub fn sent_messages(sink: &transport::TestSink) -> Vec<Message> {
        let bytes = sink.0.lock().unwrap().clone();
        let mut decoder = transport::FrameDecoder::new();
        decoder.push(&bytes);
        let mut out = Vec::new();
        while let Some(body) = decoder.next_body() {
            if let Some(msg) = Message::from_json(&body) {
                out.push(msg);
            }
        }
        out
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
                        kind: other,
                        // Surface an error result as `None` (no location).
                        result: if error.is_some() { None } else { result },
                    }),
                }
            }
            // `$/progress` drives the status line (rust-analyzer's indexing
            // phase, notably); other notifications are ignored for now (no
            // diagnostics UI yet).
            Message::Notification { method, params } => {
                if method == "$/progress" && running.server.on_progress(&params) {
                    self.status = match running.server.progress_line() {
                        Some(line) => Some(format!("LSP: {language} — {line}")),
                        None => Some(format!("LSP: {language} ready")),
                    };
                }
                None
            }
            // `window/workDoneProgress/create` just needs an empty ack before
            // the server will stream `$/progress` for that token; other
            // server-to-client requests are ignored (no dynamic registration).
            Message::Request { id, method, .. } => {
                if method == "window/workDoneProgress/create" {
                    let _ = running
                        .conn
                        .send(&protocol::response_body(&id, Value::Null));
                }
                None
            }
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

/// Languages whose server (clangd) needs a compilation database for
/// cross-file navigation; used to hint when none is discoverable.
fn needs_compile_db(language: &str) -> bool {
    matches!(language, "c" | "cpp")
}

/// Whether a `compile_commands.json` exists in any ancestor directory of
/// `path`, or in an ancestor's `build/` — the same places clangd searches.
fn compile_db_near(path: &Path) -> bool {
    path.ancestors().skip(1).any(|dir| {
        dir.join("compile_commands.json").exists()
            || dir.join("build/compile_commands.json").exists()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir for one test, removed by the caller.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vybim-lsp-test-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn compile_db_found_in_ancestor_or_its_build_dir() {
        let dir = temp_dir("db-search");
        let src = dir.join("proj/src");
        std::fs::create_dir_all(&src).unwrap();
        let file = src.join("main.c");

        // Nothing anywhere: not found.
        assert!(!compile_db_near(&file));

        // In an ancestor (the project root): found.
        std::fs::write(dir.join("proj/compile_commands.json"), "[]").unwrap();
        assert!(compile_db_near(&file));

        // Only in an ancestor's build/: also found.
        std::fs::remove_file(dir.join("proj/compile_commands.json")).unwrap();
        std::fs::create_dir_all(dir.join("proj/build")).unwrap();
        std::fs::write(dir.join("proj/build/compile_commands.json"), "[]").unwrap();
        assert!(compile_db_near(&file));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_clangd_languages_need_a_compile_db() {
        assert!(needs_compile_db("c"));
        assert!(needs_compile_db("cpp"));
        assert!(!needs_compile_db("rust"));
        assert!(!needs_compile_db("python"));
    }
}
