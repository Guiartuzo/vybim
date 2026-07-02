//! JSON-RPC message shapes and the char⇄UTF-16 position conversion at the LSP
//! boundary. Payloads are carried as `serde_json::Value` so this layer stays
//! protocol-generic; feature code deserializes into `lsp_types` structs.

use lsp_types::Position;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::buffer::Buffer;

/// A JSON-RPC request id. LSP permits a number or a string; we always *issue*
/// numbers, but must accept either when correlating a response.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Debug)]
#[serde(untagged)]
pub enum RequestId {
    Num(i64),
    Str(String),
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Num(n)
    }
}

/// A monotonic source of request ids, one per server.
#[derive(Default, Debug)]
pub struct IdSource(i64);

impl IdSource {
    pub fn next(&mut self) -> RequestId {
        self.0 += 1;
        RequestId::Num(self.0)
    }
}

/// An incoming message from the server, classified from the raw JSON-RPC.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A response to a request we issued (`result` xor `error`).
    Response {
        id: RequestId,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// A server-initiated notification (no id).
    Notification { method: String, params: Value },
    /// A server-initiated request (expects a response). v1 ignores these.
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
}

/// The wire shape used only to classify an incoming message.
#[derive(Deserialize)]
struct Raw {
    id: Option<RequestId>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<Value>,
}

impl Message {
    /// Classify a decoded JSON body into a [`Message`], or `None` if it is not a
    /// well-formed JSON-RPC message.
    pub fn from_json(bytes: &[u8]) -> Option<Message> {
        let raw: Raw = serde_json::from_slice(bytes).ok()?;
        match (raw.method, raw.id) {
            (Some(method), Some(id)) => Some(Message::Request {
                id,
                method,
                params: raw.params.unwrap_or(Value::Null),
            }),
            (Some(method), None) => Some(Message::Notification {
                method,
                params: raw.params.unwrap_or(Value::Null),
            }),
            (None, Some(id)) => Some(Message::Response {
                id,
                result: raw.result,
                error: raw.error,
            }),
            (None, None) => None,
        }
    }
}

/// Serialize an outgoing request body (`jsonrpc`/`id`/`method`/`params`).
pub fn request_body(id: &RequestId, method: &str, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

/// Serialize an outgoing notification body (no id).
pub fn notification_body(method: &str, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

/// Convert a Vybim `(line, char-col)` into an LSP position (UTF-16 column).
pub fn to_lsp_position(buffer: &Buffer, line: usize, col: usize) -> Position {
    Position {
        line: line as u32,
        character: buffer.utf16_col(line, col) as u32,
    }
}

/// Convert an LSP position (UTF-16 column) into a Vybim `(line, char-col)`.
pub fn from_lsp_position(buffer: &Buffer, pos: Position) -> (usize, usize) {
    let line = pos.line as usize;
    (
        line,
        buffer.char_col_from_utf16(line, pos.character as usize),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_response_notification_and_request() {
        let resp = Message::from_json(br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#);
        assert!(matches!(resp, Some(Message::Response { .. })));

        let note = Message::from_json(br#"{"jsonrpc":"2.0","method":"$/progress","params":{}}"#);
        assert!(matches!(note, Some(Message::Notification { .. })));

        let req =
            Message::from_json(br#"{"jsonrpc":"2.0","id":2,"method":"window/showMessageRequest"}"#);
        assert!(matches!(req, Some(Message::Request { .. })));

        assert!(Message::from_json(b"not json").is_none());
    }

    #[test]
    fn id_source_is_monotonic() {
        let mut ids = IdSource::default();
        assert_eq!(ids.next(), RequestId::Num(1));
        assert_eq!(ids.next(), RequestId::Num(2));
        assert_eq!(ids.next(), RequestId::Num(3));
    }

    #[test]
    fn position_roundtrips_on_ascii() {
        let b = Buffer::from_str("hello world");
        let pos = to_lsp_position(&b, 0, 6);
        assert_eq!(
            pos,
            Position {
                line: 0,
                character: 6
            }
        );
        assert_eq!(from_lsp_position(&b, pos), (0, 6));
    }

    #[test]
    fn position_handles_accented_bmp_char() {
        // "café x": é is one char and one UTF-16 unit, so col == utf16 col, but
        // it is multi-byte in UTF-8 — proving we count units, not bytes.
        let b = Buffer::from_str("café x");
        let pos = to_lsp_position(&b, 0, 5); // just before 'x'
        assert_eq!(pos.character, 5);
        assert_eq!(from_lsp_position(&b, pos), (0, 5));
    }

    #[test]
    fn position_handles_non_bmp_surrogate_pair() {
        // "a😀b": 😀 is 1 char but 2 UTF-16 code units (a surrogate pair).
        let b = Buffer::from_str("a😀b");
        // char col 2 is just before 'b'; in UTF-16 that is unit 3 (a=1, 😀=2).
        let pos = to_lsp_position(&b, 0, 2);
        assert_eq!(pos.character, 3);
        // And back: UTF-16 unit 3 maps to char col 2.
        assert_eq!(from_lsp_position(&b, pos), (0, 2));
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let b = Buffer::from_str("hi\nthere");
        // col past end clamps to line length in UTF-16 space.
        let pos = to_lsp_position(&b, 0, 99);
        assert_eq!(pos.character, 2);
        // utf16 col past end clamps back to a valid char col.
        assert_eq!(
            from_lsp_position(
                &b,
                Position {
                    line: 1,
                    character: 99
                }
            ),
            (1, 5)
        );
    }
}
