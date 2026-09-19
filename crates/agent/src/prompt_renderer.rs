//! Pure prompt rendering primitives.
//!
//! `PromptRenderer` only accepts already-prepared strings and memory sections.
//! It deliberately has no database, router, tool manager, or cache fields so
//! rendering cannot accidentally acquire new runtime side effects.

use haven_common::prompts::{MAIN_SYSTEM_PROMPT, render};
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};

use crate::compactor::estimate_tokens;

pub const MEMORY_START: &str = haven_common::prompts::MEMORY_FENCE_START;
pub const MEMORY_END: &str = haven_common::prompts::MEMORY_FENCE_END;

/// Facts + episodes rendered for system-prompt injection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySections {
    pub facts: String,
    pub episodes: String,
}

/// Immutable prompt renderer. Constructing one never touches application
/// state; all methods are deterministic for their arguments.
#[derive(Debug, Clone, Copy, Default)]
pub struct PromptRenderer;

impl PromptRenderer {
    pub fn render_system(
        built_in_section: &str,
        skills_section: &str,
        mcp_section: &str,
        dynamic_context: &str,
    ) -> String {
        render(
            MAIN_SYSTEM_PROMPT,
            &[
                ("tools", built_in_section),
                ("skills", skills_section),
                ("mcps", mcp_section),
                ("dynamic_context", dynamic_context),
                (
                    "failure_diagnosis",
                    haven_common::prompts::TOOL_FAILURE_DIAGNOSIS,
                ),
                ("tool_notes", haven_common::prompts::TOOL_USAGE_NOTES),
            ],
        )
    }

    pub fn render_memory_block(sections: &MemorySections) -> String {
        if sections.facts.is_empty() && sections.episodes.is_empty() {
            return format!("{MEMORY_START}MEMORY: (none)\nreason: no_hits\n{MEMORY_END}");
        }
        let mut out = String::from(MEMORY_START);
        if sections.facts.starts_with('\n') {
            out.push_str(&sections.facts[1..]);
        } else {
            out.push_str(&sections.facts);
        }
        if !sections.episodes.is_empty() {
            out.push_str(&sections.episodes);
            if !sections.episodes.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push_str(MEMORY_END);
        out
    }

    /// Replace the first closed MEMORY fence after the stable-instructions
    /// marker. Other prompt text is returned byte-for-byte unchanged.
    pub fn patch_system_memory(system_prompt: &str, new_memory_block: &str) -> String {
        const CURRENT_CLOSER: &str = "End of stable instructions.\n";
        let Some(closer_at) = system_prompt.find(CURRENT_CLOSER) else {
            return system_prompt.to_owned();
        };
        let after = closer_at + CURRENT_CLOSER.len();
        let tail = &system_prompt[after..];
        if let Some((rel_start, rel_end)) = find_first_closed_fence(tail, MEMORY_START, MEMORY_END)
        {
            return splice(
                system_prompt,
                after + rel_start,
                after + rel_end,
                new_memory_block,
            );
        }
        if new_memory_block.is_empty() {
            return system_prompt.to_owned();
        }
        if tail.contains(haven_common::prompts::SESSION_CONTEXT_FENCE_START) {
            format!("{system_prompt}{new_memory_block}")
        } else {
            splice(system_prompt, after, after, new_memory_block)
        }
    }

    /// Patch the system message in-place without inspecting or rebuilding any
    /// non-system canonical content.
    pub fn patch_canonical_memory_fence(
        &self,
        canonical: &mut [CanonicalMessage],
        new_memory_block: &str,
    ) -> bool {
        let Some(sys) = canonical.first_mut() else {
            return false;
        };
        if sys.role != CanonicalRole::System {
            return false;
        }
        for part in &mut sys.content {
            if let ContentPart::Text(text) = part {
                let patched = Self::patch_system_memory(text, new_memory_block);
                let changed = *text != patched;
                *text = patched;
                return changed;
            }
        }
        false
    }

    pub fn cap_memory_sections_to_tokens(
        mut sections: MemorySections,
        max_tokens: u32,
    ) -> MemorySections {
        let total_tokens =
            estimate_tokens(&sections.facts).saturating_add(estimate_tokens(&sections.episodes));
        if total_tokens <= max_tokens {
            return sections;
        }
        let facts_tokens = estimate_tokens(&sections.facts);
        let episodes_tokens = estimate_tokens(&sections.episodes);
        let facts_budget = if episodes_tokens == 0 {
            max_tokens
        } else {
            max_tokens
                .saturating_mul(facts_tokens)
                .checked_div(total_tokens)
                .unwrap_or(1)
                .max(1)
        };
        let episodes_budget = max_tokens.saturating_sub(facts_budget).max(1);
        sections.facts = truncate_lines_to_token_budget(&sections.facts, facts_budget);
        sections.episodes = truncate_lines_to_token_budget(&sections.episodes, episodes_budget);
        sections
    }
}

fn truncate_lines_to_token_budget(text: &str, max_tokens: u32) -> String {
    if text.is_empty() || estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }
    let mut out = String::new();
    for line in text.lines() {
        let candidate = format!("{out}{line}\n");
        if estimate_tokens(&candidate) > max_tokens {
            if out.is_empty() {
                return truncate_to_token_budget(line, max_tokens);
            }
            break;
        }
        out = candidate;
    }
    out
}

fn truncate_to_token_budget(text: &str, max_tokens: u32) -> String {
    if max_tokens == 0 || estimate_tokens(text) <= max_tokens {
        return if max_tokens == 0 {
            String::new()
        } else {
            text.to_string()
        };
    }
    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let middle = (low + high).div_ceil(2);
        let candidate: String = chars[..middle].iter().collect();
        if estimate_tokens(&candidate) <= max_tokens {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    chars[..low].iter().collect()
}

fn splice(s: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(s.len() - (end - start) + replacement.len());
    out.push_str(&s[..start]);
    out.push_str(replacement);
    out.push_str(&s[end..]);
    out
}

fn find_first_closed_fence(
    region: &str,
    start_marker: &str,
    end_marker: &str,
) -> Option<(usize, usize)> {
    let start = region.find(start_marker)?;
    let rel_end = region[start..].find(end_marker)?;
    Some((start, start + rel_end + end_marker.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_memory_has_explicit_reason() {
        let rendered = PromptRenderer::render_memory_block(&MemorySections::default());
        assert!(rendered.contains("reason: no_hits"));
        assert!(rendered.starts_with(MEMORY_START));
        assert!(rendered.ends_with(MEMORY_END));
    }
}
