use crate::tool_run_output::ToolRunOutputTail;

/// Decoded stream text retained within its byte cap.
pub(crate) struct CappedStreamText {
    pub(crate) text: String,
    pub(crate) overflowed: bool,
}

/// Retained stream bytes plus overflow and read-error state.
pub(crate) struct CappedStreamRead {
    pub(crate) bytes: Vec<u8>,
    pub(crate) overflowed: bool,
    pub(crate) error: Option<std::io::Error>,
}

/// Terminate a child process together with its whole process tree. On
/// Windows, dropping a tokio Child only terminates the direct process; the
/// real command (a grandchild of the cmd.exe/powershell.exe wrapper) would
/// survive as an orphan otherwise.
pub(crate) async fn kill_process_tree(pid: u32) {
    #[cfg(windows)]
    {
        if let Ok(mut actionkill) = tokio::process::Command::new("actionkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .kill_on_drop(true)
            .spawn()
        {
            let _ = actionkill.wait().await;
        }
    }
    #[cfg(not(windows))]
    let _ = pid;
}

/// Read a child stdout/stderr stream into a String, capping at `max_bytes`
/// so runaway output cannot exhaust memory. After the cap is reached the
/// remaining bytes are still read and discarded: closing the pipe read end
/// early can make the child fail writes (broken pipe) and flip its exit code.
/// When `tail` is given, every decoded chunk is also appended to the shared
/// bounded live-output tail (for `tool_run:output` preview events).
pub(crate) async fn read_stream_text_capped<R>(
    stdout: Option<R>,
    max_bytes: usize,
    tail: Option<ToolRunOutputTail>,
) -> CappedStreamText
where
    R: tokio::io::AsyncRead + Unpin,
{
    // Carry incomplete trailing UTF-8 bytes across read boundaries so a
    // multi-byte char split between two chunks is not mojibake'd in the live
    // tail preview. Non-UTF-8 streams (legacy GBK tools) are decoded lossily
    // per chunk and the carry is reset, so it never grows unbounded.
    let mut pending = Vec::new();
    let read = drain_stream_with_byte_cap(stdout, max_bytes, |chunk| {
        let Some(tail) = tail.as_ref() else {
            return;
        };
        pending.extend_from_slice(chunk);
        match std::str::from_utf8(&pending) {
            Ok(s) => {
                if !s.is_empty() {
                    tail.append_text(s);
                }
                pending.clear();
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if error.error_len().is_none() && pending.len() - valid_len <= 3 {
                    // Incomplete trailing sequence: flush the valid prefix
                    // and keep the remnant for the next read.
                    if valid_len > 0 {
                        let valid_text =
                            String::from_utf8_lossy(&pending[..valid_len]).into_owned();
                        tail.append_text(&valid_text);
                    }
                    pending.drain(..valid_len);
                } else {
                    // Not UTF-8 (e.g. GBK): decode the whole chunk lossily
                    // and reset the carry.
                    tail.append_bytes(&pending);
                    pending.clear();
                }
            }
        }
    })
    .await;
    // Shell and background ToolRun output keep their existing best-effort
    // behavior; SkillRunner uses the byte-level result to surface read errors.
    CappedStreamText {
        text: haven_common::encoding::decode_lossy(&read.bytes),
        overflowed: read.overflowed,
    }
}

/// Drain a stream to EOF while retaining at most `max_bytes`. Bytes past the
/// cap are discarded rather than closing the pipe, so the child can continue
/// writing and finish normally. `on_chunk` observes all bytes, including the
/// discarded tail, for bounded live previews.
pub(crate) async fn drain_stream_with_byte_cap<R, F>(
    stdout: Option<R>,
    max_bytes: usize,
    mut on_chunk: F,
) -> CappedStreamRead
where
    R: tokio::io::AsyncRead + Unpin,
    F: FnMut(&[u8]),
{
    use tokio::io::AsyncReadExt;
    let Some(mut stream) = stdout else {
        return CappedStreamRead {
            bytes: Vec::new(),
            overflowed: false,
            error: None,
        };
    };
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let mut overflowed = false;
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                on_chunk(&tmp[..n]);
                let room = max_bytes.saturating_sub(buf.len());
                if room == 0 {
                    // Cap reached: keep draining until EOF so the child can
                    // finish writing normally; only the reported text is capped.
                    overflowed = true;
                    continue;
                }
                let take = n.min(room);
                buf.extend_from_slice(&tmp[..take]);
                if take < n {
                    overflowed = true;
                }
            }
            Err(error) => {
                return CappedStreamRead {
                    bytes: buf,
                    overflowed,
                    error: Some(error),
                };
            }
        }
    }
    CappedStreamRead {
        bytes: buf,
        overflowed,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{drain_stream_with_byte_cap, read_stream_text_capped};
    use crate::tool_run_output::ToolRunOutputPort;
    use std::time::Duration;

    #[tokio::test]
    async fn test_read_stream_text_capped_under_cap() {
        let output = read_stream_text_capped(Some(&b"hello"[..]), 8192, None).await;
        assert_eq!(output.text, "hello");
        assert!(!output.overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_text_capped_none() {
        let output = read_stream_text_capped::<&[u8]>(None, 8192, None).await;
        assert_eq!(output.text, "");
        assert!(!output.overflowed);
    }

    #[tokio::test]
    async fn test_read_stream_text_capped_over_cap() {
        let data = vec![b'x'; 1000];
        let output = read_stream_text_capped(Some(&data[..]), 100, None).await;
        assert_eq!(output.text.len(), 100);
        assert!(output.overflowed);
    }

    #[tokio::test]
    async fn capped_reader_discards_excess_bytes_and_keeps_draining() {
        use tokio::io::AsyncWriteExt;

        let (mut writer, reader) = tokio::io::duplex(32);
        let writer_task = tokio::spawn(async move {
            let chunk = [b'x'; 256];
            for _ in 0..128 {
                writer.write_all(&chunk).await.unwrap();
            }
        });

        let read = tokio::time::timeout(
            Duration::from_secs(2),
            drain_stream_with_byte_cap(Some(reader), 100, |_| {}),
        )
        .await
        .expect("reader should drain the full stream");
        tokio::time::timeout(Duration::from_secs(2), writer_task)
            .await
            .expect("writer should not block after the retained-output cap")
            .unwrap();

        assert_eq!(read.bytes, vec![b'x'; 100]);
        assert!(read.overflowed);
        assert!(read.error.is_none());
    }

    #[tokio::test]
    async fn test_read_stream_text_capped_appends_tail() {
        let tail = ToolRunOutputPort::new().new_tail().await;
        let output =
            read_stream_text_capped(Some(&b"hello tail"[..]), 8192, Some(tail.clone())).await;
        assert_eq!(output.text, "hello tail");
        assert_eq!(tail.snapshot().as_str(), "hello tail");
        // A second chunk appends (multi-chunk tee).
        read_stream_text_capped(Some(&b" more"[..]), 8192, Some(tail.clone())).await;
        assert_eq!(tail.snapshot().as_str(), "hello tail more");
    }

    #[tokio::test]
    async fn test_read_stream_text_capped_tail_carries_split_multibyte() {
        // 8191 ASCII + a 3-byte UTF-8 char: the first 8192-byte read splits the
        // char (lead byte only), the second read finishes it. The live tail must
        // still show the char intact, not GBK-fallback mojibake.
        let tail = ToolRunOutputPort::new().new_tail().await;
        let mut content = "a".repeat(8191);
        content.push('中');
        read_stream_text_capped(Some(content.as_bytes()), 10_000, Some(tail.clone())).await;
        let snapshot = tail.snapshot();
        let t = snapshot.as_str();
        assert!(
            t.ends_with('中'),
            "tail must keep the split char intact, got: {:?}",
            &t[t.len().saturating_sub(40)..]
        );
        assert!(!t.contains('\u{FFFD}'), "no replacement chars in tail");
    }
}
