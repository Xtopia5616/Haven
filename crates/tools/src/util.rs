use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

/// Serialize `{ "<list_key>": items, "count": total }` and drop trailing items
/// until the JSON fits `max_chars`. Returns `(value, truncated)`; the value
/// already carries the `truncated` flag when applicable.
///
/// Items are dropped from the tail, so callers should pre-sort with the least
/// important entries last. The split point is found by binary search over the
/// serialized prefix length (monotonic in the kept count) — O(n log n) total —
/// and only the final kept slice is cloned into the returned value.
pub fn json_list_within_budget(
    list_key: &str,
    items: Vec<Value>,
    total: usize,
    max_chars: usize,
) -> (Value, bool) {
    let best = best_split_size(list_key, &items, total, max_chars);
    let truncated = best < items.len() || best < total;
    let mut obj = Map::new();
    obj.insert(list_key.to_string(), Value::Array(items[..best].to_vec()));
    obj.insert("count".to_string(), Value::from(total));
    let mut value = Value::Object(obj);
    if truncated {
        value["truncated"] = Value::Bool(true);
    }
    (value, truncated)
}

/// Borrowed view of `{ list_key: [items], count: total }` so split-size probing
/// never deep-clones the item list.
struct SnapshotRef<'a> {
    list_key: &'a str,
    items: &'a [Value],
    total: usize,
}

impl<'a> Serialize for SnapshotRef<'a> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(Some(2))?;
        m.serialize_entry(self.list_key, self.items)?;
        m.serialize_entry("count", &self.total)?;
        m.end()
    }
}

/// Largest `k` in `[1, items.len()]` whose serialized form fits `max_chars`.
/// Serialized length is monotonic in `k` for non-empty items, so a binary
/// search finds the split in O(log n) probes; each probe serializes a
/// borrowed slice (no clone).
fn best_split_size(list_key: &str, items: &[Value], total: usize, max_chars: usize) -> usize {
    let mut lo = 1usize;
    let mut hi = items.len();
    let mut best = 1;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let snap = SnapshotRef {
            list_key,
            items: &items[..mid],
            total,
        };
        let serialized = serde_json::to_string(&snap).unwrap_or_default();
        if serialized.len() <= max_chars {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }
    // Even a single item doesn't fit; return 1 so the caller still gets one
    // entry rather than an empty list.
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_fits_without_truncation() {
        let items = vec![json!({"name": "a"})];
        let (value, truncated) = json_list_within_budget("items", items, 1, 1000);
        assert!(!truncated);
        assert_eq!(value["count"], 1);
        assert!(value["truncated"].is_null());
    }

    #[test]
    fn test_shrinks_to_budget() {
        let items: Vec<Value> = (0..100)
            .map(|i| json!({"name": format!("var_{}", i), "value": "x".repeat(200)}))
            .collect();
        let (value, truncated) = json_list_within_budget("items", items, 100, 1000);
        assert!(truncated);
        assert_eq!(value["count"], 100);
        let kept = value["items"].as_array().unwrap();
        assert!(kept.len() < 100, "list should shrink, kept {}", kept.len());
        assert!(!kept.is_empty());
        assert_eq!(value["truncated"], true);
    }

    #[test]
    fn test_single_item_too_big_still_returns_it() {
        // A single item cannot be shrunk; it is returned whole (not flagged,
        // since nothing was dropped) rather than silently dropped.
        let items = vec![json!({"value": "x".repeat(5000)})];
        let (value, truncated) = json_list_within_budget("items", items, 1, 1000);
        assert!(!truncated);
        assert_eq!(value["items"].as_array().unwrap().len(), 1);
        assert_eq!(value["items"][0]["value"].as_str().unwrap().len(), 5000);
    }
}
