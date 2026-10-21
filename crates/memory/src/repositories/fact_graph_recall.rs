//! Bounded one-hop reads over the canonical memory fact graph.

use crate::db::Database;
use crate::repositories::fact_query::{FACT_COLS_ALIASED, fact_from_row, sort_facts_effective};
use crate::repositories::facts::Fact;
use haven_common::types::is_canonical_id;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use std::collections::HashSet;

pub(crate) const MAX_GRAPH_SEED_FACTS: usize = 16;
const MAX_GRAPH_NODE_ROOTS: usize = 16;
const MAX_GRAPH_CANDIDATES: usize = 80;

impl Database {
    /// Read facts adjacent to the concept nodes of a bounded seed set.
    ///
    /// The generic `user` node is excluded because expanding it would turn one
    /// personal fact into a scan over the user's full profile. Callers still
    /// apply visibility filtering before exposing returned facts.
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

        let mut seen = HashSet::new();
        let seed_ids: Vec<String> = seed_fact_ids
            .iter()
            .take(MAX_GRAPH_SEED_FACTS)
            .filter(|id| seen.insert((*id).clone()))
            .cloned()
            .collect();
        if seed_ids.is_empty() {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            seed_ids.iter().all(|id| is_canonical_id(id, "fact")),
            "graph recall seed IDs must be canonical fact-* IDs"
        );

        let seed_values = (1..=seed_ids.len())
            .map(|index| format!("(?{index}, {})", index - 1))
            .collect::<Vec<_>>()
            .join(", ");
        let subject_param = seed_ids.len() + 1;
        let kind_param = subject_param + 1;
        let limit_param = kind_param + 1;
        let seed_nodes_limit = MAX_GRAPH_NODE_ROOTS;
        let sql = format!(
            "WITH seed_ids(id, ordinal) AS (VALUES {seed_values}),
             seed_nodes(node_id, ordinal) AS (
                 SELECT f.subject_id, s.ordinal * 2
                 FROM seed_ids s JOIN facts f ON f.id = s.id
                 UNION ALL
                 SELECT f.object_id, s.ordinal * 2 + 1
                 FROM seed_ids s JOIN facts f ON f.id = s.id
             ),
             root_nodes(node_id) AS (
                 SELECT n.id
                 FROM seed_nodes s JOIN memory_nodes n ON n.id = s.node_id
                 WHERE n.kind = 'concept'
                 GROUP BY n.id
                 ORDER BY MIN(s.ordinal)
                 LIMIT {seed_nodes_limit}
             ),
             adjacent_ids(id) AS (
                 SELECT adjacent.id
                 FROM root_nodes n
                 JOIN facts adjacent INDEXED BY idx_facts_subject_id
                   ON adjacent.subject_id = n.node_id
                 UNION
                 SELECT adjacent.id
                 FROM root_nodes n
                 JOIN facts adjacent INDEXED BY idx_facts_object_id
                   ON adjacent.object_id = n.node_id
             )
             SELECT {FACT_COLS_ALIASED}
             FROM facts f JOIN adjacent_ids a ON a.id = f.id
             WHERE f.id NOT IN (SELECT id FROM seed_ids)
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
        let limit = limit.min(MAX_GRAPH_CANDIDATES);
        let mut params: Vec<Value> = seed_ids.into_iter().map(Value::Text).collect();
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
        params.push(Value::Integer(limit as i64));

        let conn = self.try_conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), fact_from_row)?;
        let mut facts = Vec::new();
        for row in rows {
            facts.push(row?);
        }
        sort_facts_effective(&mut facts);
        Ok(facts)
    }
}

#[cfg(test)]
mod tests {
    use super::Database;
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
    fn graph_recall_caps_results_and_returns_empty_for_missing_seed() {
        let db = Database::open_in_memory().unwrap();
        let (seed, _, _) = seed_and_neighbors(&db);
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
        }

        let limited = db
            .related_facts_for_recall(&[seed.clone()], 1_000, None, None)
            .unwrap();
        assert_eq!(limited.len(), 80);

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
