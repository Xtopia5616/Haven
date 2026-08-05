//! All LLM-facing prompt templates in the project, kept in one place.
//!
//! Edit prompt wording here. Crates that need dynamic sections (tools,
//! facts, history, ...) assemble those in code and inject them through
//! [`render`] into [`MAIN_SYSTEM_PROMPT`], or interpolate them directly
//! around the plain constants below.

/// Fill `{name}` placeholders in `template` with the given values.
///
/// Replacement is single-pass: values already inserted are never scanned
/// again, so a value may safely contain literal `{...}` text (e.g. user
/// input) without it being expanded. Unknown placeholders are left as-is.
pub fn render(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        let Some(start) = rest.find('{') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 1..];
        if let Some(end) = after_open.find('}') {
            let key = &after_open[..end];
            match values.iter().find(|(k, _)| *k == key) {
                Some((_, value)) => out.push_str(value),
                None => {
                    out.push('{');
                    out.push_str(key);
                    out.push('}');
                }
            }
            rest = &after_open[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            break;
        }
    }
    out
}

/// Main ReAct agent system prompt (default_model).
///
/// Placeholders:
/// - `{tools}` — built-in tool index (non-empty)
/// - `{skills}` — installable skills index, or empty
/// - `{mcps}` — available MCP servers index, or empty
/// - `{facts}` — user facts block, or empty
/// - `{preferences}` — preference summary line, or empty
/// - `{task}` — current task description
/// - `{context}` — additional conversation context block, or empty
/// - `{history}` — "Steps so far" block, or empty
pub const MAIN_SYSTEM_PROMPT: &str = "\
You are Haven, a PC voice assistant. You help users accomplish tasks using available tools. \
Stay interactive: when the goal is unclear, a decision matters, or you keep trying on your own, \
use `ask` to consult the user instead of guessing.\n\
\n\
Available tools:\n\
You have access to the following built-in tools:\n\
\n\
{tools}{skills}{mcps}{facts}{preferences}\n\
Guidelines:\n\
1. Think step by step. Decide what to do, then call the right tool.\n\
2. After each tool call you will receive the result. Use it to decide next.\n\
3. When the task is complete, respond with a summary of what was done.\n\
4. If no tool is needed, answer directly.\n\
5. Never call the same tool with identical parameters twice in a row.\n\
6. shell(background: true) returns a job_id immediately; the job's final output is delivered back to you automatically as context when it finishes — do not poll it.\n\
7. shell(silent: true) hides the command output from the user, but you still see it.\n\
8. Calling ask pauses the task until the user replies; their answer is injected as context for the next step.\n\
9. Calling notify sends the user a desktop notification (in-app toast + Windows) without pausing the task. Use it to alert them about background progress or something they should check.\n\
\n\
Current task: {task}\n\
\n\
{context}{history}\n\
What is your next step?\n";

/// Conversation title generator (small_model).
pub const TITLE_SYSTEM_PROMPT: &str = "You are a title generator. Generate a concise title (max 6 words, in the same language as the conversation) for this conversation. Respond with ONLY the title, no quotes, no punctuation, no explanation.";

/// User fact extraction (balanced_model). Expects a JSON array in response.
pub const FACT_EXTRACTION_SYSTEM_PROMPT: &str = "Extract factual information about the user from the conversation. Return a JSON array where each element has: \"subject\" (always \"user\"), \"predicate\" (short key: name, likes, dislikes, uses, works_at, project_path, etc.), \"object\" (the value), \"tags\" (array of: identity, preference, workspace, project), \"confidence\" (0.5-1.0). Only extract clear, explicit facts the user stated. If no facts found, return []. Respond with ONLY the JSON array, no markdown, no explanation.";

/// Conversation compaction summary prefix (default_model). The transcript
/// is appended after this text.
pub const CONVERSATION_SUMMARY_PROMPT: &str =
    "Summarize this conversation. Keep key facts, decisions, and context:\n\n";

/// LLM speech-to-text transcription (audio_model).
pub const STT_SYSTEM_PROMPT: &str = "You are a speech-to-text engine. Transcribe the audio verbatim in the speaker's language. Output only the transcription text, no commentary.";

/// Image analysis (image_model via the router's vision role).
pub const IMAGE_ANALYSIS_SYSTEM_PROMPT: &str = "You are analyzing an image. Describe what it shows and transcribe any visible text. Respond concisely in the user's language.";

/// File content summarizer (small_model).
pub const FILE_SUMMARY_SYSTEM_PROMPT: &str = "You are a summarizer. Summarize the following file content concisely. Focus on the most important points, structure, and notable details. Respond in the same language as the content. Keep the summary under 250 words.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_fills_placeholders() {
        let out = render("{a}-{b}", &[("a", "1"), ("b", "2")]);
        assert_eq!(out, "1-2");
    }

    #[test]
    fn render_leaves_unknown_placeholders() {
        let out = render("{a}{missing}", &[("a", "x")]);
        assert_eq!(out, "x{missing}");
    }

    #[test]
    fn render_does_not_rescan_inserted_values() {
        let out = render("pre{a}post", &[("a", "{b}"), ("b", "NOPE")]);
        assert_eq!(out, "pre{b}post");
    }

    #[test]
    fn main_prompt_has_expected_structure() {
        let out = render(
            MAIN_SYSTEM_PROMPT,
            &[
                ("tools", "- read_file: read a file\n"),
                ("task", "test task"),
                ("skills", ""),
                ("mcps", ""),
                ("facts", ""),
                ("preferences", ""),
                ("context", ""),
                ("history", ""),
            ],
        );
        assert!(out.contains("You are Haven"));
        assert!(out.contains("Available tools:"));
        assert!(out.contains("- read_file: read a file"));
        assert!(out.contains("Current task: test task"));
        assert!(out.ends_with("What is your next step?\n"));
    }
}
