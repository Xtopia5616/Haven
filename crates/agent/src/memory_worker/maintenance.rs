use std::collections::HashMap;

use haven_common::prompts::{CONTRADICTION_ARBITRATE_SYSTEM_PROMPT, predicate_merge_system_prompt};
use haven_memory::MemoryMaintenanceStore;
use haven_memory::recall::MemoryRetriever;
use haven_memory::repositories::facts::{
    CANONICAL_MERGE_TARGETS, Fact, is_canonical_merge_target, normalize_predicate,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::fact_extraction::extract_json_array;
use crate::fact_inference::{
    ContradictionDemoteProposal, PredicateMergeProposal, format_contradiction_groups,
    gate_contradiction_demote, gate_predicate_merge,
};
use crate::memory_inference::MemoryInferencePort;
use crate::memory_service::MemoryService;

pub(super) struct MemoryMaintenancePass<'a> {
    maintenance_store: &'a MemoryMaintenanceStore,
    inference: &'a dyn MemoryInferencePort,
    inference_semaphore: &'a Semaphore,
    memory: &'a MemoryService,
    fact_inference_enabled: bool,
}

impl<'a> MemoryMaintenancePass<'a> {
    pub(super) fn new(
        maintenance_store: &'a MemoryMaintenanceStore,
        inference: &'a dyn MemoryInferencePort,
        inference_semaphore: &'a Semaphore,
        memory: &'a MemoryService,
        fact_inference_enabled: bool,
    ) -> Self {
        Self {
            maintenance_store,
            inference,
            inference_semaphore,
            memory,
            fact_inference_enabled,
        }
    }

    pub(super) async fn run(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run_memory_maintenance_with_cancellation(cancellation)
            .await
    }

    async fn run_memory_maintenance_with_cancellation(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        ensure_memory_maintenance_active(cancellation)?;
        let mut cleaned = 0u64;
        let mut failures = Vec::new();

        let dedup = self.maintenance_store.dedup_facts(cancellation).await;
        ensure_memory_maintenance_active(cancellation)?;
        match dedup {
            Ok(count) => cleaned += count,
            Err(error) => {
                tracing::warn!("memory maintenance: dedup_facts failed: {}", error);
                failures.push(format!("dedup_facts: {error}"));
            }
        }

        let sensitive = self
            .maintenance_store
            .delete_sensitive_facts(cancellation)
            .await;
        ensure_memory_maintenance_active(cancellation)?;
        match sensitive {
            Ok(count) => cleaned += count,
            Err(error) => {
                tracing::error!(
                    "memory maintenance: delete_sensitive_facts failed: {}",
                    error
                );
                failures.push(format!("delete_sensitive_facts: {error}"));
            }
        }

        // X5: demote recent polarity / single-valued losers that slipped past
        // upsert (age-capped), before low-confidence flush can delete them in
        // the same pass.
        let contradictions = self
            .maintenance_store
            .resolve_contradictions(cancellation)
            .await;
        ensure_memory_maintenance_active(cancellation)?;
        match contradictions {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(
                        "memory maintenance: resolved {} contradictory fact(s)",
                        count
                    );
                }
                cleaned += count;
            }
            Err(error) => {
                tracing::warn!(
                    "memory maintenance: resolve_contradictions failed: {}",
                    error
                );
                failures.push(format!("resolve_contradictions: {error}"));
            }
        }

        let low_confidence = self
            .maintenance_store
            .flush_low_confidence(0.3, cancellation)
            .await;
        ensure_memory_maintenance_active(cancellation)?;
        match low_confidence {
            Ok(count) => cleaned += count,
            Err(error) => {
                tracing::warn!("memory maintenance: flush_low_confidence failed: {}", error);
                failures.push(format!("flush_low_confidence: {error}"));
            }
        }

        let pruned = self
            .maintenance_store
            .prune_orphaned_embeddings(cancellation)
            .await;
        ensure_memory_maintenance_active(cancellation)?;
        match pruned {
            Ok(count) => cleaned += count,
            Err(error) => {
                tracing::warn!(
                    "memory maintenance: prune_orphaned_embeddings failed: {}",
                    error
                );
                failures.push(format!("prune_orphaned_embeddings: {error}"));
            }
        }

        let orphan_cursors = self
            .maintenance_store
            .cleanup_orphan_extraction_cursors(cancellation)
            .await;
        ensure_memory_maintenance_active(cancellation)?;
        if let Err(error) = orphan_cursors {
            tracing::warn!(
                "memory maintenance: cleanup_orphan_extraction_cursors failed: {}",
                error
            );
            failures.push(format!("cleanup_orphan_extraction_cursors: {error}"));
        }

        // provenance_item_id is FK ON DELETE SET NULL; opaque
        // provenance_record_id values are intentional transcript refs. Still
        // normalize empty record ids.
        let source_refs = self
            .maintenance_store
            .cleanup_orphan_source_refs(cancellation)
            .await;
        ensure_memory_maintenance_active(cancellation)?;
        match source_refs {
            Ok(count) => cleaned += count,
            Err(error) => {
                tracing::warn!(
                    "memory maintenance: cleanup_orphan_source_refs failed: {}",
                    error
                );
                failures.push(format!("cleanup_orphan_source_refs: {error}"));
            }
        }

        if !failures.is_empty() {
            anyhow::bail!("memory maintenance failed: {}", failures.join("; "));
        }

        ensure_memory_maintenance_active(cancellation)?;
        let merged = if self.fact_inference_enabled {
            self.merge_predicates_with_llm().await
        } else {
            0
        };
        ensure_memory_maintenance_active(cancellation)?;
        // Alias merges can create new single-valued multi-object conflicts;
        // re-run the rule keeper before LLM arbitration so merge-created
        // pairs get the same user>inferred / confidence treatment.
        let resolved_after_merge = if merged > 0 {
            match self
                .maintenance_store
                .resolve_contradictions(cancellation)
                .await
            {
                Ok(count) => count,
                Err(_error) if cancellation.is_some_and(CancellationToken::is_cancelled) => {
                    anyhow::bail!("memory maintenance cancelled")
                }
                Err(error) => {
                    tracing::warn!(
                        "memory maintenance: post-merge resolve_contradictions failed: {}",
                        error
                    );
                    0
                }
            }
        } else {
            0
        };
        ensure_memory_maintenance_active(cancellation)?;
        let arbitrated = if self.fact_inference_enabled {
            self.arbitrate_contradictions_with_llm().await
        } else {
            0
        };
        ensure_memory_maintenance_active(cancellation)?;
        // Catch up on vector indexing too, so memory that accumulated while
        // the embedding model was unconfigured gets indexed once it is set up.
        // Rebuild LSH only when the side table lags the embedding rows (M5).
        self.memory.embed_new_memory().await;
        self.memory.rebuild_lsh_if_lagging().await;
        ensure_memory_maintenance_active(cancellation)?;
        Ok(cleaned
            .saturating_add(merged)
            .saturating_add(resolved_after_merge)
            .saturating_add(arbitrated))
    }

    /// Maintenance LLM pass (X5): residual contradiction groups after the
    /// rule engine, with `source_ref` snippets as evidence. LLM runs outside
    /// the DB lock; demotes are gated then applied in a separate blocking
    /// call. Returns rows demoted.
    pub(super) async fn arbitrate_contradictions_with_llm(&self) -> u64 {
        if !self.inference.is_fast_chat_configured().await {
            return 0;
        }
        let groups = match self.maintenance_store.list_ambiguous_contradictions().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: list_ambiguous_contradictions failed: {}",
                    e
                );
                return 0;
            }
        };
        let groups: Vec<_> = groups
            .into_iter()
            .filter_map(|mut group| {
                group.facts.retain(MemoryRetriever::visible_fact);
                (!group.facts.is_empty()).then_some(group)
            })
            .collect();
        if groups.is_empty() {
            return 0;
        }
        let listing = format_contradiction_groups(&groups);
        let user_content = format!(
            "Conflict groups (JSON):\n{listing}\n\nPropose demotions for residual contradictions."
        );

        let _permit = match self.inference_semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => return 0,
        };
        let response = match self
            .inference
            .fast_chat(CONTRADICTION_ARBITRATE_SYSTEM_PROMPT, &user_content)
            .await
        {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: contradiction arbitrate LLM failed: {}",
                    e
                );
                return 0;
            }
        };
        if response.trim().is_empty() {
            return 0;
        }
        let json_str = extract_json_array(&response);
        let proposals: Vec<ContradictionDemoteProposal> = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: failed to parse contradiction demote JSON: {}",
                    e
                );
                return 0;
            }
        };

        let allowed: HashMap<String, &Fact> = groups
            .iter()
            .flat_map(|g| g.facts.iter().map(|f| (f.id.clone(), f)))
            .collect();
        let mut demote_ids: Vec<String> = Vec::new();
        for p in proposals.into_iter().take(20) {
            if let Some(id) = gate_contradiction_demote(&p, &allowed, &groups)
                && !demote_ids.iter().any(|x| x == &id)
            {
                demote_ids.push(id);
            }
        }
        if demote_ids.is_empty() {
            return 0;
        }
        match self.maintenance_store.demote_fact_ids(demote_ids).await {
            Ok(count) => {
                if count > 0 {
                    tracing::info!(
                        "memory maintenance: LLM demoted {} contradictory fact(s)",
                        count
                    );
                }
                count
            }
            Err(_) => 0,
        }
    }

    /// Maintenance LLM pass (M6): propose predicate alias merges and apply
    /// only gated rewrites. LLM runs outside the DB lock; SQL apply is a
    /// separate blocking call. Returns rows rewritten.
    pub(super) async fn merge_predicates_with_llm(&self) -> u64 {
        if !self.inference.is_fast_chat_configured().await {
            return 0;
        }
        let counts = match self.maintenance_store.list_predicate_counts().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("memory maintenance: list_predicate_counts failed: {}", e);
                return 0;
            }
        };
        // Only bother the model when some keys still need collapsing: either
        // a known alias spelling, or a free-form non-canonical predicate.
        let needs_merge = counts.iter().any(|entry| {
            let normalized = normalize_predicate(&entry.predicate);
            normalized != entry.predicate || !is_canonical_merge_target(&normalized)
        });
        if !needs_merge || counts.len() < 2 {
            return 0;
        }
        let listing = counts
            .iter()
            .take(60)
            .map(|entry| format!("{}\t{}", entry.predicate, entry.row_count))
            .collect::<Vec<_>>()
            .join("\n");
        let user_content = format!(
            "Predicate counts (predicate\\trows):\n{listing}\n\nPropose merges for free-form keys onto canonical ones."
        );

        let _permit = match self.inference_semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => return 0,
        };
        let merge_prompt = predicate_merge_system_prompt(CANONICAL_MERGE_TARGETS);
        let response = match self.inference.fast_chat(&merge_prompt, &user_content).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!("memory maintenance: predicate merge LLM failed: {}", e);
                return 0;
            }
        };
        if response.trim().is_empty() {
            return 0;
        }
        let json_str = extract_json_array(&response);
        let proposals: Vec<PredicateMergeProposal> = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: failed to parse predicate merge JSON: {}",
                    e
                );
                return 0;
            }
        };

        let mut accepted: Vec<(String, String)> = Vec::new();
        for p in proposals.into_iter().take(20) {
            if let Some((from, to)) = gate_predicate_merge(&p) {
                accepted.push((from, to));
            }
        }
        if accepted.is_empty() {
            return 0;
        }
        let mut total = 0u64;
        for (from, to) in accepted {
            match self.maintenance_store.rewrite_predicate(&from, &to).await {
                Ok(count) => {
                    if count > 0 {
                        tracing::info!(
                            "memory maintenance: rewrote predicate '{}' → '{}' ({} rows)",
                            from,
                            to,
                            count
                        );
                        total += count;
                    }
                }
                Err(error) => tracing::warn!(
                    "memory maintenance: rewrite_predicate {}→{} failed: {}",
                    from,
                    to,
                    error
                ),
            }
        }
        total
    }
}

fn ensure_memory_maintenance_active(
    cancellation: Option<&CancellationToken>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !cancellation.is_some_and(CancellationToken::is_cancelled),
        "memory maintenance cancelled"
    );
    Ok(())
}
