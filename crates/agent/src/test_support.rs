use haven_memory::{Database, MAX_MEMORY_OUTBOX_PAGE_SIZE};

pub(crate) fn list_pending_fact_extraction_rows(
    db: &Database,
) -> anyhow::Result<Vec<(String, bool, i64)>> {
    let Some(high_water) = db.pending_fact_extraction_high_water()? else {
        return Ok(Vec::new());
    };

    let mut after_key = None;
    let mut rows = Vec::new();
    loop {
        let page = db.pending_fact_extractions_page(
            after_key.as_deref(),
            &high_water,
            MAX_MEMORY_OUTBOX_PAGE_SIZE,
        )?;
        if page.is_empty() {
            break;
        }
        after_key = page.last().map(|marker| marker.key.clone());
        for marker in page {
            let state = marker
                .state
                .map_err(|error| anyhow::anyhow!("invalid marker {}: {error}", marker.key))?;
            rows.push((
                marker.session_id,
                state.bypass_throttle,
                state.event_sequence,
            ));
        }
    }
    Ok(rows)
}

pub(crate) fn list_pending_summary_extraction_rows(
    db: &Database,
) -> anyhow::Result<Vec<(String, String)>> {
    let Some(high_water) = db.pending_summary_extraction_high_water()? else {
        return Ok(Vec::new());
    };

    let mut after_key = None;
    let mut rows = Vec::new();
    loop {
        let page = db.pending_summary_extractions_page(
            after_key.as_deref(),
            &high_water,
            MAX_MEMORY_OUTBOX_PAGE_SIZE,
        )?;
        if page.is_empty() {
            break;
        }
        after_key = page.last().map(|marker| marker.key.clone());
        for marker in page {
            marker
                .state
                .map_err(|error| anyhow::anyhow!("invalid marker {}: {error}", marker.key))?;
            rows.push((marker.session_id, marker.episode_id));
        }
    }
    Ok(rows)
}
