//! Bounded two-hop reads over the canonical memory fact graph.

use crate::db::Database;
use crate::repositories::fact_query::{FACT_COLS_ALIASED, fact_from_row, sort_facts_effective};
use crate::repositories::facts::{Fact, filter_visible_facts};
use haven_common::types::is_canonical_id;
use rusqlite::Connection;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use std::collections::HashSet;

pub(crate) const MAX_GRAPH_SEED_FACTS: usize = 16;
const MAX_GRAPH_NODE_ROOTS: usize = 16;
const MAX_GRAPH_CANDIDATES: usize = 80;
const MAX_GRAPH_FIRST_HOP_CANDIDATES: usize = 60;

impl Database {
    /// Read facts adjacent to a bounded seed set, then expand one more hop from
    /// visible first-hop facts. Generic `user` nodes are never traversed.
    ///
    /// The total candidate budget is shared across both hops. First-hop facts
    /// have priority; second-hop facts can use the reserved portion, while any
    /// unused capacity falls back to additional first-hop candidates.
    pub(crate) fn related_facts_for_recall(
        &self,
        seed_fact_ids: &[String],
        limit: usize,
        fact_subject: Option<&str>,
        memory_kind: Option<&str>,
    ) -> anyhow::Result<Vec<Fact>> {
        if seed_fact_ids.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut seen_seeds = HashSet::new();
        let seed_ids: Vec<String> = seed_fact_ids
            .iter()
            .take(MAX_GRAPH_SEED_FACTS)
            .filter(|id| seen_seeds.insert((*id).clone()))
            .cloned()
            .collect();
        if seed_ids.is_empty() {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            seed_ids.iter().all(|id| is_canonical_id(id, "fact")),
            "graph recall seed IDs must be canonical fact-* IDs"
        );

        let candidate_limit = limit.min(MAX_GRAPH_CANDIDATES);
        let first_hop_limit =
            (candidate_limit - candidate_limit / 4).clamp(1, MAX_GRAPH_FIRST_HOP_CANDIDATES);
        let conn = self.try_conn()?;
        let first_roots = concept_nodes_for_facts(&conn, &seed_ids, &HashSet::new())?;
        if first_roots.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch the whole bounded pool once. Lower-ranked first-hop facts can
        // fill unused second-hop capacity without another graph scan.
        let first_hop_pool = adjacent_facts(
            &conn,
            &first_roots,
            &seed_ids,
            candidate_limit,
            fact_subject,
            memory_kind,
        )?;
        let primary_count = first_hop_pool.len().min(first_hop_limit);
        let primary_raw = &first_hop_pool[..primary_count];
        let fallback_raw = &first_hop_pool[primary_count..];
        let mut related = filter_visible_facts(primary_raw.iter().cloned());

        let second_hop_budget = candidate_limit / 4;
        if second_hop_budget > 0 && !related.is_empty() {
            let primary_ids: Vec<String> =
                related.iter().map(|fact| fact.fact_id.clone()).collect();
            let expanded_nodes: HashSet<String> = first_roots.iter().cloned().collect();
            let second_roots = concept_nodes_for_facts(&conn, &primary_ids, &expanded_nodes)?;
            if !second_roots.is_empty() {
                let mut excluded_fact_ids = seed_ids;
                excluded_fact_ids.extend(first_hop_pool.iter().map(|fact| fact.fact_id.clone()));
                let second_hop = adjacent_facts(
                    &conn,
                    &second_roots,
                    &excluded_fact_ids,
                    second_hop_budget,
                    fact_subject,
                    memory_kind,
                )?;
                related.extend(filter_visible_facts(second_hop));
            }
        }

        let remaining = candidate_limit.saturating_sub(related.len());
        if remaining > 0 {
            related.extend(
                filter_visible_facts(fallback_raw.iter().cloned())
                    .into_iter()
                    .take(remaining),
            );
        }
        Ok(related)
    }
}

/// Return up to 16 unique concept endpoints in stable fact/endpoint order.
/// Excluding previously expanded nodes prevents a second hop from looping
/// back through its seed frontier.
fn concept_nodes_for_facts(
    conn: &Connection,
    fact_ids: &[String],
    excluded_node_ids: &HashSet<String>,
) -> anyhow::Result<Vec<String>> {
    if fact_ids.is_empty() {
        return Ok(Vec::new());
    }
    let values = fact_ids
        .iter()
        .enumerate()
        .map(|(index, _)| format!("(?{}, {})", index + 1, index * 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "WITH fact_ids(id, ordinal) AS (VALUES {values}),
         candidate_nodes(node_id, ordinal) AS (
             SELECT f.subject_id, i.ordinal
             FROM fact_ids i JOIN facts f ON f.id = i.id
             UNION ALL
             SELECT f.object_id, i.ordinal + 1
             FROM fact_ids i JOIN facts f ON f.id = i.id
         )
         SELECT c.node_id
         FROM candidate_nodes c JOIN memory_nodes n ON n.id = c.node_id
         WHERE n.kind = 'concept'
         GROUP BY c.node_id
         ORDER BY MIN(c.ordinal)"
    );
    let params = fact_ids
        .iter()
        .cloned()
        .map(Value::Text)
        .collect::<Vec<_>>();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(0))?;
    let mut roots = Vec::new();
    let mut seen = excluded_node_ids.clone();
    for row in rows {
        let node_id = row?;
        anyhow::ensure!(
            is_canonical_id(&node_id, "node"),
            "graph recall found a noncanonical node-* ID"
        );
        if seen.insert(node_id.clone()) {
            roots.push(node_id);
        }
    }
    roots.truncate(MAX_GRAPH_NODE_ROOTS);
    Ok(roots)
}

fn adjacent_facts(
    conn: &Connection,
    root_node_ids: &[String],
    excluded_fact_ids: &[String],
    limit: usize,
    fact_subject: Option<&str>,
    memory_kind: Option<&str>,
) -> anyhow::Result<Vec<Fact>> {
    if root_node_ids.is_empty() || excluded_fact_ids.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let roots_values = root_node_ids
        .iter()
        .enumerate()
        .map(|(index, _)| format!("(?{}, {})", index + 1, index))
        .collect::<Vec<_>>()
        .join(", ");
    let excluded_offset = root_node_ids.len();
    let excluded_values = excluded_fact_ids
        .iter()
        .enumerate()
        .map(|(index, _)| format!("(?{})", excluded_offset + index + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let subject_param = excluded_offset + excluded_fact_ids.len() + 1;
    let kind_param = subject_param + 1;
    let limit_param = kind_param + 1;
    let sql = format!(
        "WITH root_nodes(node_id, ordinal) AS (VALUES {roots_values}),
         excluded_ids(id) AS (VALUES {excluded_values}),
         adjacent_raw(id) AS (
             SELECT adjacent.id
             FROM root_nodes n
             JOIN facts adjacent INDEXED BY idx_facts_subject_id
               ON adjacent.subject_id = n.node_id
             UNION
             SELECT adjacent.id
             FROM root_nodes n
             JOIN facts adjacent INDEXED BY idx_facts_object_id
               ON adjacent.object_id = n.node_id
         ),
         adjacent_ids(id) AS (
             SELECT id FROM adjacent_raw
         )
         SELECT {FACT_COLS_ALIASED}
         FROM facts f JOIN adjacent_ids a ON a.id = f.id
         WHERE f.id NOT IN (SELECT id FROM excluded_ids)
           AND (?{subject_param} IS NULL OR f.subject = ?{subject_param})
           AND (?{kind_param} IS NULL OR f.memory_kind = ?{kind_param})
         ORDER BY f.confidence * CASE
             WHEN f.source = 'user' OR f.predicate IN
               ('name','birthday','email','phone','city','country','timezone')
             THEN 1.0 ELSE f.durability END DESC,
             COALESCE(f.last_seen_at, f.created_at) DESC,
             f.created_at DESC, f.id ASC
         LIMIT ?{limit_param}"
    );
    let mut params = Vec::with_capacity(limit_param);
    params.extend(root_node_ids.iter().cloned().map(Value::Text));
    params.extend(excluded_fact_ids.iter().cloned().map(Value::Text));
    params.push(
        fact_subject
            .map(str::to_string)
            .map_or(Value::Null, Value::Text),
    );
    params.push(
        memory_kind
            .map(str::to_string)
            .map_or(Value::Null, Value::Text),
    );
    params.push(Value::Integer(limit.min(MAX_GRAPH_CANDIDATES) as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), fact_from_row)?;
    let mut facts = Vec::new();
    for row in rows {
        facts.push(row?);
    }
    sort_facts_effective(&mut facts);
    Ok(facts)
}

#[cfg(test)]
mod tests {
    use super::{Database, MAX_GRAPH_CANDIDATES};
    use crate::repositories::facts::MemoryFactKind;

    fn seed_and_neighbors(db: &Database) -> (String, String, String) {
        let seed = db
            .insert_fact("user", "employer", "Acme", "user", 1.0, &[])
            .unwrap();
        let subject_neighbor = db
            .insert_fact("Acme", "headquartered_in", "Seattle", "inferred", 0.9, &[])
            .unwrap();
        let object_neighbor = db
            .insert_fact("Microsoft", "acquired", "Acme", "inferred", 0.8, &[])
            .unwrap();
        (
            seed.fact_id,
            subject_neighbor.fact_id,
            object_neighbor.fact_id,
        )
    }

    #[test]
    fn graph_recall_reads_both_edge_directions_without_returning_seeds() {
        let db = Database::open_in_memory().unwrap();
        let (seed, subject_neighbor, object_neighbor) = seed_and_neighbors(&db);

        let related = db
            .related_facts_for_recall(&[seed.clone()], 10, None, None)
            .unwrap();
        let ids: Vec<&str> = related.iter().map(|fact| fact.fact_id.as_str()).collect();

        assert!(ids.contains(&subject_neighbor.as_str()));
        assert!(ids.contains(&object_neighbor.as_str()));
        assert!(!ids.contains(&seed.as_str()));
    }

    #[test]
    fn graph_recall_expands_at_most_two_hops() {
        let db = Database::open_in_memory().unwrap();
        let seed = db
            .insert_fact("user", "employer", "Acme", "user", 1.0, &[])
            .unwrap();
        let first_hop = db
            .insert_fact("Acme", "headquartered_in", "Seattle", "inferred", 0.9, &[])
            .unwrap();
        let second_hop = db
            .insert_fact("Seattle", "in_state", "Washington", "inferred", 0.8, &[])
            .unwrap();
        let third_hop = db
            .insert_fact("Washington", "capital", "Olympia", "inferred", 0.7, &[])
            .unwrap();

        let related = db
            .related_facts_for_recall(&[seed.fact_id.clone()], 20, None, None)
            .unwrap();
        let ids: Vec<&str> = related.iter().map(|fact| fact.fact_id.as_str()).collect();

        assert!(ids.contains(&first_hop.fact_id.as_str()));
        assert!(ids.contains(&second_hop.fact_id.as_str()));
        assert!(!ids.contains(&seed.fact_id.as_str()));
        assert!(!ids.contains(&third_hop.fact_id.as_str()));
    }

    #[test]
    fn graph_recall_does_not_expand_through_hidden_first_hop_facts() {
        let db = Database::open_in_memory().unwrap();
        let seed = db
            .insert_fact("user", "employer", "Acme", "user", 1.0, &[])
            .unwrap();
        let hidden_bridge = db
            .insert_fact("Acme", "api_key", "SecretProject", "inferred", 1.0, &[])
            .unwrap();
        let downstream = db
            .insert_fact("SecretProject", "launch_date", "2027", "inferred", 1.0, &[])
            .unwrap();

        let related = db
            .related_facts_for_recall(&[seed.fact_id], 20, None, None)
            .unwrap();
        let ids: Vec<&str> = related.iter().map(|fact| fact.fact_id.as_str()).collect();

        assert!(!ids.contains(&hidden_bridge.fact_id.as_str()));
        assert!(!ids.contains(&downstream.fact_id.as_str()));
    }

    #[test]
    fn graph_recall_applies_subject_and_memory_kind_scopes() {
        let db = Database::open_in_memory().unwrap();
        let (seed, _, _) = seed_and_neighbors(&db);
        let user_neighbor = db
            .insert_fact("user", "works_at", "Acme", "user", 1.0, &[])
            .unwrap();

        let scoped = db
            .related_facts_for_recall(&[seed.clone()], 10, Some("user"), Some("profile"))
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].fact_id, user_neighbor.fact_id);

        let wrong_kind = db
            .related_facts_for_recall(&[seed], 10, Some("user"), Some("procedural"))
            .unwrap();
        assert!(wrong_kind.is_empty());
        assert_eq!(scoped[0].memory_kind, MemoryFactKind::Profile);
    }

    #[test]
    fn graph_recall_caps_total_results_and_returns_empty_for_missing_seed() {
        let db = Database::open_in_memory().unwrap();
        let (seed, _, _) = seed_and_neighbors(&db);
        let mut second_hop_ids = Vec::new();
        for index in 0..100 {
            db.insert_fact(
                &format!("Acme-{index}"),
                "parent_company",
                "Acme",
                "inferred",
                0.5,
                &[],
            )
            .unwrap();
            let second_hop = db
                .insert_fact(
                    &format!("Acme-{index}"),
                    "headquartered_in",
                    &format!("City-{index}"),
                    "inferred",
                    0.5,
                    &[],
                )
                .unwrap();
            second_hop_ids.push(second_hop.fact_id);
        }

        let limited = db
            .related_facts_for_recall(&[seed.clone()], 1_000, None, None)
            .unwrap();
        assert_eq!(limited.len(), MAX_GRAPH_CANDIDATES);
        assert!(
            second_hop_ids
                .iter()
                .any(|id| limited.iter().any(|fact| &fact.fact_id == id))
        );

        let missing = db
            .related_facts_for_recall(
                &["fact-0123456789abcdef0123456789abcdef".into()],
                10,
                None,
                None,
            )
            .unwrap();
        assert!(missing.is_empty());
        assert!(
            db.related_facts_for_recall(&[seed], 0, None, None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn graph_recall_requires_canonical_seed_fact_ids() {
        let db = Database::open_in_memory().unwrap();
        let error = db
            .related_facts_for_recall(&["fact-short".into()], 10, None, None)
            .unwrap_err();
        assert!(error.to_string().contains("canonical fact-*"));
    }
}
