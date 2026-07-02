//! LSP transport: `Content-Length` framing and a subprocess connection whose
//! stdout is pumped onto the shared `AppEvent` channel by a reader thread —
//! the same shape as `TerminalPane::spawn`, but framed JSON-RPC instead of
//! terminal bytes.

use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use serde_json::Value;

use crate::app::AppEvent;
use crate::lsp::protocol::Message;
use crate::lsp::registry::ServerCmd;

/// Wrap a JSON body in an LSP `Content-Length` frame.
pub fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// Incrementally reassembles `Content-Length`-framed messages from a byte
/// stream that may split frames at arbitrary boundaries.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append freshly read bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete JSON body, or `None` if one is not yet buffered.
    pub fn next_body(&mut self) -> Option<Vec<u8>> {
        // Find the end of the header block.
        let header_end = find_subslice(&self.buf, b"\r\n\r\n")?;
        let header = &self.buf[..header_end];
        let len = parse_content_length(header)?;
        let body_start = header_end + 4;
        if self.buf.len() < body_start + len {
            return None; // body not fully arrived yet
        }
        let body = self.buf[body_start..body_start + len].to_vec();
        self.buf.drain(..body_start + len);
        Some(body)
    }
}

fn parse_content_length(header: &[u8]) -> Option<usize> {
    let text = std::str::from_utf8(header).ok()?;
    for line in text.split("\r\n") {
        if let Some(rest) = line
            .strip_prefix("Content-Length:")
            .or_else(|| line.strip_prefix("content-length:"))
        {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A live connection to a language server subprocess.
/// A live connection to a language server subprocess. The writer is boxed so
/// tests can substitute an in-memory sink for the child's stdin.
pub struct Connection {
    /// Held to own the subprocess handle for the connection's lifetime; the
    /// server is stopped via `shutdown`/`exit`, not by dropping this.
    #[allow(dead_code)]
    child: Option<Child>,
    writer: Box<dyn Write + Send>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

impl Connection {
    /// Launch the server for `cmd` in `root`, piping stdio. A reader thread
    /// frames stdout messages and forwards each as `AppEvent::Lsp(server_id,
    /// msg)`, emitting `AppEvent::LspExit(server_id)` on EOF. stderr is drained
    /// to `log` for debugging (best effort).
    pub fn spawn(
        server_id: usize,
        cmd: &ServerCmd,
        root: &Path,
        tx: Sender<AppEvent>,
        log: Option<std::fs::File>,
    ) -> io::Result<Connection> {
        let mut child = Command::new(&cmd.program)
            .args(&cmd.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take();

        // Reader thread: frame stdout → AppEvent::Lsp, then LspExit on EOF.
        thread::spawn(move || {
            let mut reader = stdout;
            let mut decoder = FrameDecoder::new();
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        decoder.push(&chunk[..n]);
                        while let Some(body) = decoder.next_body() {
                            if let Some(msg) = Message::from_json(&body)
                                && tx.send(AppEvent::Lsp(server_id, msg)).is_err()
                            {
                                return; // receiver gone; stop
                            }
                        }
                    }
                }
            }
            let _ = tx.send(AppEvent::LspExit(server_id));
        });

        // stderr drain thread (best effort logging).
        if let (Some(mut stderr), Some(mut log)) = (stderr, log) {
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk) {
                    if n == 0 || log.write_all(&chunk[..n]).is_err() {
                        break;
                    }
                }
            });
        }

        Ok(Connection {
            child: Some(child),
            writer: Box::new(stdin),
        })
    }

    /// A connection with no subprocess whose writes land in `sink`, for tests.
    #[cfg(test)]
    pub fn for_test(sink: TestSink) -> Connection {
        Connection {
            child: None,
            writer: Box::new(sink),
        }
    }

    /// Frame and write an outgoing message body to the server's stdin.
    /// Fire-and-forget: small writes on the main thread for v1.
    pub fn send(&mut self, body: &Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(body)?;
        self.writer.write_all(&frame(&bytes))?;
        self.writer.flush()
    }
}

/// An in-memory, shareable sink capturing framed bytes written to a test
/// [`Connection`], so a test can decode what the client sent.
#[cfg(test)]
#[derive(Clone, Default)]
pub struct TestSink(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl Write for TestSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_single_frame() {
        let mut d = FrameDecoder::new();
        d.push(&frame(br#"{"a":1}"#));
        assert_eq!(d.next_body().unwrap(), br#"{"a":1}"#);
        assert!(d.next_body().is_none());
    }

    #[test]
    fn decodes_header_split_across_reads() {
        let full = frame(br#"{"hello":"world"}"#);
        let (a, b) = full.split_at(10); // split mid-header
        let mut d = FrameDecoder::new();
        d.push(a);
        assert!(d.next_body().is_none()); // header incomplete
        d.push(b);
        assert_eq!(d.next_body().unwrap(), br#"{"hello":"world"}"#);
    }

    #[test]
    fn decodes_body_split_across_reads() {
        let full = frame(br#"{"n":42}"#);
        let split = full.len() - 3;
        let (a, b) = full.split_at(split); // split mid-body
        let mut d = FrameDecoder::new();
        d.push(a);
        assert!(d.next_body().is_none());
        d.push(b);
        assert_eq!(d.next_body().unwrap(), br#"{"n":42}"#);
    }

    #[test]
    fn decodes_multiple_messages_in_one_buffer() {
        let mut d = FrameDecoder::new();
        let mut stream = frame(br#"{"i":1}"#);
        stream.extend(frame(br#"{"i":2}"#));
        d.push(&stream);
        assert_eq!(d.next_body().unwrap(), br#"{"i":1}"#);
        assert_eq!(d.next_body().unwrap(), br#"{"i":2}"#);
        assert!(d.next_body().is_none());
    }

    #[test]
    fn frame_roundtrips_through_the_decoder_as_a_message() {
        let body = protocol_body();
        let mut d = FrameDecoder::new();
        d.push(&frame(&serde_json::to_vec(&body).unwrap()));
        let got = d.next_body().unwrap();
        let msg = Message::from_json(&got).unwrap();
        assert!(matches!(msg, Message::Response { .. }));
    }

    fn protocol_body() -> Value {
        serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}})
    }
}
