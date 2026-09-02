use std::sync::{Arc, Mutex};

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

/// Update `last` from the live-output tail when content changed. Returns
/// true so callers can emit `action:output` / `agent:tool_output`. Compare
/// by value so a sliding capped window (same length, new content) still
/// notifies the UI.
pub(crate) fn take_tail_if_changed(tail: &Mutex<String>, last: &mut String) -> bool {
    let t = tail.lock().unwrap();
    if t.as_str() == last.as_str() {
        return false;
    }
    last.clone_from(&t);
    true
}

/// Append a decoded chunk to the shared live-output tail, keeping it bounded
/// to the last `max_chars` characters (dropping from the front). `max_chars`
/// comes from `context_limits.background_job_tail_max_chars`.
pub(crate) fn append_tail(tail: &Mutex<String>, chunk: &[u8], max_chars: usize) {
    let text = haven_common::encoding::decode_lossy(chunk);
    if text.is_empty() {
        return;
    }
    let mut t = tail.lock().unwrap();
    t.push_str(&text);
    while t.len() > max_chars {
        let overflow = t.len() - max_chars;
        let cut = t
            .char_indices()
            .nth(overflow)
            .map(|(i, _)| i)
            .unwrap_or(t.len());
        t.drain(..cut);
    }
}

/// Read a child stdout/stderr stream into a String, capping at `max_bytes`
/// so runaway output cannot exhaust memory. After the cap is reached the
/// remaining bytes are still read and discarded: closing the pipe read end
/// early can make the child fail writes (broken pipe) and flip its exit code.
/// When `tail` is given, every decoded chunk is also appended to the shared
/// bounded live-output tail (for `action:output` preview events).
/// Returns `(text, overflowed)`.
pub(crate) async fn read_stream_capped<R>(
    stdout: Option<R>,
    max_bytes: usize,
    tail: Option<Arc<Mutex<String>>>,
    tail_max_chars: usize,
) -> (String, bool)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut stream) = stdout else {
        return (String::new(), false);
    };
    let mut buf = Vec::with_capacity(max_bytes.min(8192));
    let mut tmp = [0u8; 8192];
    let mut overflowed = false;
    // Carry incomplete trailing UTF-8 bytes across read boundaries so a
    // multi-byte char split between two chunks is not mojibake'd in the live
    // tail preview. Non-UTF-8 streams (legacy GBK tools) are decoded lossily
    // per chunk and the carry is reset, so it never grows unbounded.
    let mut pending = Vec::new();
    loop {
        match stream.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                if let Some(t) = &tail {
                    pending.extend_from_slice(&tmp[..n]);
                    match std::str::from_utf8(&pending) {
                        Ok(s) => {
                            if !s.is_empty() {
                                append_tail(t, s.as_bytes(), tail_max_chars);
                            }
                            pending.clear();
                        }
                        Err(e) => {
                            let valid = e.valid_up_to();
                            if e.error_len().is_none() && pending.len() - valid <= 3 {
                                // Incomplete trailing sequence: flush the valid
                                // prefix and keep the remnant for the next read.
                                if valid > 0 {
                                    append_tail(t, &pending[..valid], tail_max_chars);
                                }
                                pending.drain(..valid);
                            } else {
                                // Not UTF-8 (e.g. GBK): decode the whole chunk
                                // lossily and reset the carry.
                                append_tail(t, &pending, tail_max_chars);
                                pending.clear();
                            }
                        }
                    }
                }
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
            Err(_) => break,
        }
    }
    (haven_common::encoding::decode_lossy(&buf), overflowed)
}
