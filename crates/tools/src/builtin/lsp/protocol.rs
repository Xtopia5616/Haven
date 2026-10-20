use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct LspRpcError {
    pub(super) code: i64,
}

#[derive(Debug)]
pub(super) struct LspRequestOutcome {
    pub(super) result: Option<Value>,
    pub(super) error: Option<LspRpcError>,
    pub(super) pushed_diagnostics: Option<Value>,
}

pub(super) struct LspConnection<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
    workspace_uri: String,
    document_uri: Option<String>,
    pushed_diagnostics: Option<Value>,
}

impl<R, W> LspConnection<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(super) fn new(reader: R, writer: W, workspace_uri: String) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
            workspace_uri,
            document_uri: None,
            pushed_diagnostics: None,
        }
    }

    pub(super) fn set_document_uri(&mut self, uri: String) {
        self.document_uri = Some(uri);
    }

    pub(super) async fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        write_message(
            &mut self.writer,
            &json!({"jsonrpc":"2.0", "method":method, "params":params}),
        )
        .await
    }

    pub(super) async fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> anyhow::Result<LspRequestOutcome> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        write_message(
            &mut self.writer,
            &json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}),
        )
        .await?;
        loop {
            let message = read_message(&mut self.reader).await?;
            if self.capture_notification(&message) {
                continue;
            }
            if message.get("method").and_then(Value::as_str).is_some()
                && message.get("id").is_some()
            {
                self.respond_to_server_request(&message).await?;
                continue;
            }
            if message.get("id") != Some(&json!(id)) {
                continue;
            }
            let outcome = if let Some(error) = message.get("error") {
                LspRequestOutcome {
                    result: None,
                    error: Some(LspRpcError {
                        code: error.get("code").and_then(Value::as_i64).unwrap_or(-32000),
                    }),
                    pushed_diagnostics: self.pushed_diagnostics.clone(),
                }
            } else {
                LspRequestOutcome {
                    result: message.get("result").cloned(),
                    error: None,
                    pushed_diagnostics: self.pushed_diagnostics.clone(),
                }
            };
            return Ok(outcome);
        }
    }

    pub(super) async fn wait_for_pushed_diagnostics(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<Option<Value>> {
        if self.pushed_diagnostics.is_some() {
            return Ok(self.pushed_diagnostics.clone());
        }
        let wait = async {
            loop {
                let message = read_message(&mut self.reader).await?;
                if self.capture_notification(&message) {
                    if self.pushed_diagnostics.is_some() {
                        return Ok(self.pushed_diagnostics.clone());
                    }
                    continue;
                }
                if message.get("method").and_then(Value::as_str).is_some()
                    && message.get("id").is_some()
                {
                    self.respond_to_server_request(&message).await?;
                }
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(result) => result,
            Err(_) => Ok(None),
        }
    }

    fn capture_notification(&mut self, message: &Value) -> bool {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return false;
        };
        // Server requests carry an id and must be answered by the client;
        // only notifications can be consumed without a response.
        if message.get("id").is_some() {
            return false;
        }
        if method != "textDocument/publishDiagnostics" {
            return true;
        }
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let uri = params
            .get("uri")
            .or_else(|| params.pointer("/textDocument/uri"))
            .and_then(Value::as_str);
        if uri == self.document_uri.as_deref() {
            self.pushed_diagnostics = Some(params);
        }
        true
    }

    async fn respond_to_server_request(&mut self, message: &Value) -> anyhow::Result<()> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let result = match method {
            "workspace/configuration" => {
                let count = message
                    .pointer("/params/items")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_default();
                Value::Array(vec![Value::Null; count])
            }
            "workspace/workspaceFolders" => Value::Array(vec![json!({
                "uri": self.workspace_uri,
                "name": self.workspace_uri.rsplit('/').next().unwrap_or("workspace")
            })]),
            "client/registerCapability" => Value::Null,
            "window/showMessageRequest" => Value::Null,
            _ => {
                write_message(
                    &mut self.writer,
                    &json!({
                        "jsonrpc":"2.0",
                        "id":id,
                        "error":{"code":-32601,"message":"client method is not supported"}
                    }),
                )
                .await?;
                return Ok(());
            }
        };
        write_message(
            &mut self.writer,
            &json!({"jsonrpc":"2.0", "id":id, "result":result}),
        )
        .await
    }
}

async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Value,
) -> anyhow::Result<()> {
    let body = serde_json::to_vec(message)?;
    anyhow::ensure!(
        body.len() <= MAX_MESSAGE_BYTES,
        "LSP message exceeds the size limit"
    );
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<Value> {
    let mut header_bytes = 0usize;
    let mut content_length = None;
    loop {
        let mut line = Vec::new();
        loop {
            let byte = reader.read_u8().await?;
            header_bytes += 1;
            anyhow::ensure!(
                header_bytes <= MAX_HEADER_BYTES,
                "LSP header exceeds the size limit"
            );
            if byte == b'\n' {
                break;
            }
            anyhow::ensure!(byte.is_ascii(), "LSP headers must use ASCII");
            line.push(byte);
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            break;
        }
        let line = std::str::from_utf8(&line)?;
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("LSP header field is malformed"))?;
        if name.eq_ignore_ascii_case("content-length") {
            anyhow::ensure!(
                content_length.is_none(),
                "LSP message has duplicate Content-Length"
            );
            let length = value
                .trim()
                .parse::<usize>()
                .map_err(|_| anyhow::anyhow!("LSP Content-Length is invalid"))?;
            anyhow::ensure!(
                length <= MAX_MESSAGE_BYTES,
                "LSP message exceeds the size limit"
            );
            content_length = Some(length);
        } else if name.eq_ignore_ascii_case("content-type") {
            let content_type = value.to_ascii_lowercase();
            anyhow::ensure!(
                !content_type.contains("charset=")
                    || content_type.contains("charset=utf-8")
                    || content_type.contains("charset=utf8"),
                "LSP server selected an unsupported content encoding"
            );
        }
    }
    let length = content_length.ok_or_else(|| anyhow::anyhow!("LSP Content-Length is missing"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    let body = std::str::from_utf8(&body)?;
    Ok(serde_json::from_str(body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, split};

    #[tokio::test]
    async fn framing_uses_utf8_byte_length_and_accepts_header_order() {
        let (mut left, mut right) = duplex(1024);
        let message = json!({"text":"中文🦀"});
        let write = write_message(&mut left, &message);
        let read = async {
            let mut reader = BufReader::new(&mut right);
            read_message(&mut reader).await
        };
        let (written, decoded) = tokio::join!(write, read);
        written.unwrap();
        assert_eq!(decoded.unwrap(), message);

        let mut framed =
            b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: 7\r\n\r\n"
                .to_vec();
        framed.extend_from_slice(br#"{"x":1}"#);
        let mut reader = BufReader::new(framed.as_slice());
        assert_eq!(read_message(&mut reader).await.unwrap(), json!({"x":1}));
    }

    #[tokio::test]
    async fn request_answers_server_configuration_and_skips_notifications() {
        let (client_stream, server_stream) = duplex(4096);
        let (client_read, client_write) = split(client_stream);
        let (server_read, mut server_write) = split(server_stream);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let request = read_message(&mut reader).await.unwrap();
            assert_eq!(request["method"], "initialize");
            write_message(
                &mut server_write,
                &json!({"jsonrpc":"2.0","method":"window/logMessage","params":{"message":"ignored"}}),
            )
            .await
            .unwrap();
            write_message(
                &mut server_write,
                &json!({"jsonrpc":"2.0","id":99,"method":"workspace/configuration","params":{"items":[{},{}]}}),
            )
            .await
            .unwrap();
            let response = read_message(&mut reader).await.unwrap();
            assert_eq!(response["id"], 99);
            assert_eq!(response["result"], json!([null, null]));
            write_message(
                &mut server_write,
                &json!({"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}),
            )
            .await
            .unwrap();
        });
        let mut connection =
            LspConnection::new(client_read, client_write, "file:///workspace".into());
        let outcome = connection.request("initialize", json!({})).await.unwrap();
        assert_eq!(outcome.result.unwrap()["capabilities"], json!({}));
        assert!(outcome.error.is_none());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_oversized_content_length_before_allocation() {
        let frame = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE_BYTES + 1);
        let mut reader = BufReader::new(frame.as_bytes());
        let error = read_message(&mut reader).await.unwrap_err();
        assert!(error.to_string().contains("size limit"));
    }
}
