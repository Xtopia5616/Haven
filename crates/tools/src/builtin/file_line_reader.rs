use tokio::io::{AsyncBufReadExt, BufReader};

/// Read one line while retaining at most `cap` bytes. When a line exceeds the
/// cap, consume its remainder so the next call starts at the next line.
/// Returns `None` at EOF, otherwise the retained byte count and whether the
/// line exceeded the cap.
pub(super) async fn read_line_bounded(
    reader: &mut BufReader<tokio::fs::File>,
    buf: &mut Vec<u8>,
    cap: usize,
) -> anyhow::Result<Option<(usize, bool)>> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() {
                None
            } else {
                Some((buf.len(), false))
            });
        }
        let remaining = cap.saturating_sub(buf.len());
        if remaining == 0 {
            discard_until_newline(reader).await?;
            return Ok(Some((buf.len(), true)));
        }
        let window_len = available.len().min(remaining);
        if let Some(pos) = available[..window_len]
            .iter()
            .position(|&byte| byte == b'\n')
        {
            let take = pos + 1;
            buf.extend_from_slice(&available[..take]);
            reader.consume(take);
            return Ok(Some((buf.len(), false)));
        }
        buf.extend_from_slice(&available[..window_len]);
        reader.consume(window_len);
    }
}

async fn discard_until_newline(reader: &mut BufReader<tokio::fs::File>) -> anyhow::Result<()> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        if let Some(pos) = available.iter().position(|&byte| byte == b'\n') {
            reader.consume(pos + 1);
            return Ok(());
        }
        let len = available.len();
        reader.consume(len);
    }
}
