//! TCP connection management with line-delimited I/O.
//!
//! Stratum v1 uses newline-delimited JSON over TCP. This module provides a
//! wrapper around tokio's TCP stream that handles buffered reading and writing
//! of complete JSON-RPC messages.

use super::error::{StratumError, StratumResult};
use super::messages::JsonRpcMessage;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tracing::{debug, trace};

/// Buffered connection for Stratum protocol.
///
/// Wraps a TCP stream with buffered readers/writers optimized for
/// line-delimited JSON messages. Messages are automatically serialized
/// and deserialized, with newlines added/stripped.
pub struct Connection {
    /// Buffered reader for incoming messages
    reader: BufReader<OwnedReadHalf>,

    /// Buffered writer for outgoing messages
    writer: BufWriter<OwnedWriteHalf>,

    /// Line buffer for reading messages
    line_buf: String,
}

impl Connection {
    /// Create a new connection from a TCP stream.
    pub fn new(stream: TcpStream) -> Self {
        // Split the stream for independent reading and writing
        let (read_half, write_half) = stream.into_split();

        Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
            line_buf: String::with_capacity(4096),
        }
    }

    /// Connect to a Stratum pool.
    ///
    /// Parses the URL, establishes TCP connection, and wraps it in a buffered
    /// connection. Supports both `stratum+tcp://` and plain `tcp://` schemes.
    pub async fn connect(url: &str) -> StratumResult<Self> {
        // Parse URL
        let url = url
            .strip_prefix("stratum+tcp://")
            .or_else(|| url.strip_prefix("tcp://"))
            .unwrap_or(url);

        debug!(url = %url, "Connecting to pool");

        // Connect
        let stream = TcpStream::connect(url)
            .await
            .map_err(|e| StratumError::ConnectionFailed(e.to_string()))?;

        debug!("Connected to pool");

        Ok(Self::new(stream))
    }

    /// Read one complete JSON-RPC message.
    ///
    /// Reads a newline-delimited line from the stream and deserializes it as
    /// JSON-RPC. Returns `None` if the connection is closed cleanly. Skips
    /// empty lines automatically.
    pub async fn read_message(&mut self) -> StratumResult<Option<JsonRpcMessage>> {
        loop {
            self.line_buf.clear();

            let n = self
                .reader
                .read_line(&mut self.line_buf)
                .await
                .map_err(StratumError::Io)?;

            if n == 0 {
                // EOF - connection closed
                return Ok(None);
            }

            let line = self.line_buf.trim();
            if line.is_empty() {
                // Empty line, skip and read next
                continue;
            }

            trace!(rx = %line, "Received message");

            let msg = serde_json::from_str(line).map_err(|e| {
                StratumError::InvalidMessage(format!("Failed to parse JSON: {}, line: {}", e, line))
            })?;

            return Ok(Some(msg));
        }
    }

    /// Write a JSON-RPC message.
    ///
    /// Serializes the message to JSON and writes it with a trailing newline.
    /// Flushes the write buffer to ensure delivery.
    pub async fn write_message(&mut self, msg: &JsonRpcMessage) -> StratumResult<()> {
        let json = serde_json::to_string(msg)?;
        trace!(tx = %json, "Sending message");

        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn test_message_roundtrip() {
        // Create a local test server
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn server task
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut conn = Connection::new(socket);

            // Echo messages back
            while let Ok(Some(msg)) = conn.read_message().await {
                conn.write_message(&msg).await.unwrap();
            }
        });

        // Connect client
        let stream = TcpStream::connect(addr).await.unwrap();
        let mut conn = Connection::new(stream);

        // Send a message
        let request = JsonRpcMessage::request(1, "test.method", json!(["param1", "param2"]));
        conn.write_message(&request).await.unwrap();

        // Read it back
        let response = conn.read_message().await.unwrap().unwrap();
        assert_eq!(response.id(), Some(1));
        assert_eq!(response.method(), Some("test.method"));
    }
}
