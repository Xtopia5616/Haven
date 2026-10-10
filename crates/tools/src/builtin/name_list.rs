use std::collections::HashSet;

/// Preserve first-occurrence order while removing empty and duplicate names.
/// Callers normalize each domain's names before passing them here.
pub(super) fn ordered_unique_non_empty_names(
    values: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}
