//! Per-server protocol state: the initialize lifecycle, capability storage,
//! request correlation with stale-drop, and document-sync message construction.
//!
//! This layer is pure — every method returns the JSON body to send and mutates
//! only in-memory state, so the whole state machine is unit-testable without a
//! subprocess. The transport ([`super::transport`]) does the actual writing.

use std::collections::HashMap;
use std::path::Path;

use lsp_types::ServerCapabilities;
use serde_json::{Value, json};

use crate::lsp::protocol::{IdSource, RequestId, notification_body, request_body};

/// What an in-flight request was for, so its response routes to the right
/// feature. Extended as features are added ([[lsp-goto-definition]] adds the
/// definition arm's consumer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Initialize,
    Definition,
}

/// The protocol state for one language server.
#[derive(Debug)]
pub struct Server {
    pub id: usize,
    pub capabilities: Option<ServerCapabilities>,
    ids: IdSource,
    pending: HashMap<RequestId, PendingKind>,
    /// Open documents by URI → last synced version.
    open_docs: HashMap<String, i32>,
}

impl Server {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            capabilities: None,
            ids: IdSource::default(),
            pending: HashMap::new(),
            open_docs: HashMap::new(),
        }
    }

    /// The `initialize` request body (records it as a pending `Initialize`).
    pub fn initialize(&mut self, root: &Path) -> Value {
        let id = self.ids.next();
        self.pending.insert(id.clone(), PendingKind::Initialize);
        let params = json!({
            "processId": std::process::id(),
            "rootUri": file_uri(root),
            "capabilities": {
                "textDocument": {
                    "definition": { "linkSupport": true },
                    "synchronization": { "didSave": false }
                }
            },
        });
        request_body(&id, "initialize", params)
    }

    /// Handle the `initialize` response: store capabilities and return the
    /// `initialized` notification body to send next.
    pub fn on_initialize_response(&mut self, result: Option<&Value>) -> Value {
        if let Some(caps) = result.and_then(|r| r.get("capabilities")) {
            self.capabilities = serde_json::from_value(caps.clone()).ok();
        }
        notification_body("initialized", json!({}))
    }

    /// Whether the server advertised `textDocument/definition` support.
    pub fn supports_definition(&self) -> bool {
        self.capabilities
            .as_ref()
            .map(|c| c.definition_provider.is_some())
            .unwrap_or(false)
    }

    /// Issue a request of `kind`, returning its id and the body to send.
    pub fn request(
        &mut self,
        method: &str,
        params: Value,
        kind: PendingKind,
    ) -> (RequestId, Value) {
        let id = self.ids.next();
        self.pending.insert(id.clone(), kind);
        (id.clone(), request_body(&id, method, params))
    }

    /// Correlate a response id back to what it was for, removing it from the
    /// pending set. `None` for an unknown id (already superseded or bogus).
    pub fn on_response(&mut self, id: &RequestId) -> Option<PendingKind> {
        self.pending.remove(id)
    }

    /// Drop all pending requests of `kind` so their late responses are ignored
    /// (e.g. the cursor moved before a definition reply arrived).
    pub fn supersede(&mut self, kind: PendingKind) {
        self.pending.retain(|_, k| *k != kind);
    }

    /// `didOpen` for a freshly opened served document (version 1).
    pub fn did_open(&mut self, uri: &str, language_id: &str, text: &str) -> Value {
        self.open_docs.insert(uri.to_string(), 1);
        notification_body(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )
    }

    /// `didChange` carrying the full document text at the next version, or
    /// `None` if the document is not open here.
    pub fn did_change(&mut self, uri: &str, text: &str) -> Option<Value> {
        let version = self.open_docs.get_mut(uri)?;
        *version += 1;
        Some(notification_body(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": *version },
                "contentChanges": [ { "text": text } ],
            }),
        ))
    }

    /// `didClose` for a document leaving; `None` if it was not open.
    pub fn did_close(&mut self, uri: &str) -> Option<Value> {
        self.open_docs.remove(uri)?;
        Some(notification_body(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        ))
    }

    pub fn is_open(&self, uri: &str) -> bool {
        self.open_docs.contains_key(uri)
    }

    /// The `shutdown` request and `exit` notification bodies, in order.
    pub fn shutdown(&mut self) -> (Value, Value) {
        let id = self.ids.next();
        (
            request_body(&id, "shutdown", Value::Null),
            notification_body("exit", Value::Null),
        )
    }
}

/// A `file://` URI for a local path — the canonical, percent-encoded encoding
/// (so the URI we sync a document under matches the one a request carries).
pub fn file_uri(path: &Path) -> String {
    crate::lsp::protocol::path_to_uri(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served() -> Server {
        Server::new(0)
    }

    #[test]
    fn initialize_records_pending_and_response_stores_capabilities() {
        let mut s = served();
        let body = s.initialize(Path::new("/tmp/proj"));
        assert_eq!(body["method"], "initialize");
        // The initialize response carries capabilities we then store.
        let result = json!({ "capabilities": { "definitionProvider": true } });
        let initialized = s.on_initialize_response(Some(&result));
        assert_eq!(initialized["method"], "initialized");
        assert!(s.capabilities.is_some());
        assert!(s.supports_definition());
    }

    #[test]
    fn no_definition_capability_when_not_advertised() {
        let mut s = served();
        s.on_initialize_response(Some(&json!({ "capabilities": {} })));
        assert!(!s.supports_definition());
    }

    #[test]
    fn request_correlation_routes_by_id_and_removes() {
        let mut s = served();
        let (id, body) = s.request(
            "textDocument/definition",
            json!({}),
            PendingKind::Definition,
        );
        assert_eq!(body["method"], "textDocument/definition");
        assert_eq!(s.on_response(&id), Some(PendingKind::Definition));
        // Second lookup of the same id is gone.
        assert_eq!(s.on_response(&id), None);
    }

    #[test]
    fn supersede_drops_pending_of_a_kind() {
        let mut s = served();
        let (id, _) = s.request(
            "textDocument/definition",
            json!({}),
            PendingKind::Definition,
        );
        s.supersede(PendingKind::Definition);
        assert_eq!(s.on_response(&id), None); // late response ignored
    }

    #[test]
    fn document_sync_emits_in_order_with_incrementing_versions() {
        let mut s = served();
        let uri = "file:///tmp/proj/a.rs";

        let open = s.did_open(uri, "rust", "fn a() {}");
        assert_eq!(open["method"], "textDocument/didOpen");
        assert_eq!(open["params"]["textDocument"]["version"], 1);

        let c1 = s.did_change(uri, "fn a() {} // 1").unwrap();
        assert_eq!(c1["method"], "textDocument/didChange");
        assert_eq!(c1["params"]["textDocument"]["version"], 2);

        let c2 = s.did_change(uri, "fn a() {} // 2").unwrap();
        assert_eq!(c2["params"]["textDocument"]["version"], 3);

        assert!(s.is_open(uri));
        let close = s.did_close(uri).unwrap();
        assert_eq!(close["method"], "textDocument/didClose");
        assert!(!s.is_open(uri));

        // Changes/closes on an unknown document are no-ops.
        assert!(s.did_change(uri, "x").is_none());
        assert!(s.did_close(uri).is_none());
    }
}
