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
/// - `{session}` — current session description
/// - `{context}` — additional conversation context block, or empty
/// - `{history}` — "Steps so far" block, or empty
/// - `{failure_diagnosis}` — shared tool-failure guidance
///   ([`TOOL_FAILURE_DIAGNOSIS`])
/// - `{tool_notes}` — per-tool supplementary usage notes
///   ([`TOOL_USAGE_NOTES`])
pub const MAIN_SYSTEM_PROMPT: &str = "\
You are Haven, a PC agent. You help users accomplish sessions using available tools. \
Stay interactive: when the goal is unclear, a decision matters, or you keep trying on your own, \
use `ask` to consult the user instead of guessing.\n\
\n\
You have access to the following built-in tools:\n\
\n\
{tools}{skills}{mcps}{facts}\n\
Guidelines:\n\
General:\n\
1. Think step by step. Decide what to do, then call the right tool.\n\
2. After each tool call you will receive the result. Use it to decide next.\n\
3. When the session is complete, respond with a concise summary of what was done, in the same language the user is using.\n\
4. If no tool is needed, answer directly.\n\
5. Never call the same tool with identical parameters twice in a row.\n\
Shell & background actions:\n\
6. shell(background: true) returns a action_id immediately; the action's final output is delivered back to you automatically as context when it finishes — do not poll it with `action_status`. Use `actions` to see all background actions at once. The user also gets a push notification when a background action finishes.\n\
7. shell(silent: true) hides the command output from the user, but you still see it.\n\
Interaction & notifications:\n\
8. Calling ask pauses the session until the user replies; their answer is injected as context for the next step.\n\
9. Calling notify sends the user a desktop notification (in-app toast + Windows) without pausing the session. Use it to alert them about background progress or something they should check.\n\
Tool selection:\n\
10. Simple, quick sessions: use built-in tools — they are fast, lightweight, and always available.\n\
11. Complex, comprehensive sessions: prefer MCP servers and Skills — if the session matches a server or a skill in the lists above, call `load_mcp` with that server name or `load_skill` with that skill name to activate it first, then use its more powerful, specialized tools.\n\
Failure handling:\n\
12. {failure_diagnosis}\n\
\n\
{tool_notes}\n\
Current session: {session}\n\
\n\
{context}{history}\n\
What is your next step?\n";

/// Canonical tool-failure diagnosis guidance, shared by the main system
/// prompt (guideline 12, injected via the `{failure_diagnosis}` placeholder)
/// and the per-step retry nudge in the ReAct loop, so the model-visible
/// advice cannot drift between the two.
pub const TOOL_FAILURE_DIAGNOSIS: &str = "When a tool call fails, first diagnose the cause: is it an environment problem (missing command, wrong shell syntax, network/proxy, wrong path) or a logic problem? Fix the cause and retry the same approach, switching tools (e.g. curl -> aria2) if the environment requires it. Only switch to a completely different approach when the method itself is wrong.";

/// Per-tool supplementary usage guidance, rendered as a dedicated block of the
/// main system prompt (via the `{tool_notes}` placeholder). Kept separate from
/// the one-line tool index so each tool can carry richer "when to use / when
/// not to use" advice without bloating the list.
pub const TOOL_USAGE_NOTES: &str = "Tool usage notes:\n\
- ask: When anything is unclear or a decision matters, asking the user is welcome — ask instead of guessing on your own.\n\
- network: Fine for simple HTTP requests and quick fetches. For web search or heavy retrieval, prefer an MCP server (load_mcp) instead.\n\
- shell: Never run interactive commands that block waiting for input (interactive prompts, REPLs, editors, pagers, wizards) — they will hang forever because no one is there to answer. Use non-interactive flags (e.g. -y, --yes, -n) or supply all input up front instead.\n\
- shell (background actions): After launching a background action, do not wait for it or poll it. End your turn — you will be reconnected and resumed automatically with the action's output when it finishes.";

/// Conversation title generator (small_model).
pub const TITLE_SYSTEM_PROMPT: &str = "You are a title generator. Generate a concise title (max 6 words, in the same language as the conversation) for this conversation. Respond with ONLY the title, no quotes, no punctuation, no explanation.";

/// User fact extraction (balanced_model). Expects a JSON array in response.
/// The user content lists already-stored facts and a numbered conversation
/// transcript (`[N] ...`); facts reference the supporting message by number.
pub const FACT_EXTRACTION_SYSTEM_PROMPT: &str = "You extract durable, generalizable facts about the user from a conversation. Return a JSON array. Each element has these fields:\n\
- \"subject\": the entity the fact is about. Use \"user\" for facts about the person using Haven (their name, preferences, projects, tools). Use a specific entity name (project name, tool name, file path, organization) when the fact is about that entity rather than about the person — e.g. \"haven\" for \"the haven project lives at D:/Workspace/Haven\". Default to \"user\" when unsure.\n\
- \"predicate\": a short, stable key naming the attribute. Reuse keys already present in the \"Known user facts\" list (name, birthday, email, city, timezone, works_at, project_path, language, likes, dislikes, uses, verbosity, shell, os, location, etc.). One key per concept, never one key per value: use a single \"likes\" for every liked thing — never \"likes_rust\", \"likes_pizza\". Prefer an existing key over inventing a new one; only create a new key when no existing key fits.\n\
- \"object\": the value, kept short and clean. Trim surrounding whitespace and trailing fluff (\"very much\", \"as well\", \"actually\"); do not copy whole sentences.\n\
- \"tags\": use ONLY from this set — identity (stable personal attributes), preference (likes, dislikes, wants, and output habits like language/verbosity), workspace (paths, project locations, environment, tools), project (project-specific context). Default to \"preference\" when unsure; at most 2 tags per fact.\n\
- \"confidence\": a number from 0.5 to 1.0. Start at 0.6 for one explicit statement; raise toward 0.9-1.0 when the user re-confirms or states it emphatically; use 0.5 for weak or indirect signals. Brand-new facts below about 0.55 are dropped, so keep this honest.\n\
- \"durability\": a number from 0.1 to 1.0 rating how long this fact stays useful. 0.9-1.0 for stable identity and long-term context that will matter for months (name, city, workplace, core project setup); 0.5-0.7 for ongoing preferences and habits that may change over time; 0.2-0.4 for facts that are useful only in the near term or tied to a specific situation. Default to 0.5 when unsure.\n\
- \"message_index\": the [N] number of the conversation message supporting this fact; omit only when no message clearly supports it.\n\
\n\
Only extract facts that will still be true and useful weeks later, in unrelated conversations: stable identity attributes, ongoing preferences, and long-term context (projects, workspace layout, tools). Reject everything transient or one-off: current moods and busy states (\"I am busy today\", \"I love this right now\"), complaints or observations about a single session (\"the build is slow\", \"this error is annoying\"), details that only matter for the current conversation, and trivial tastes stated without intent to last (\"this font looks nice\"). When in doubt whether a fact will matter later, do not extract it.\n\
\n\
Only extract clear, explicit facts the user stated. The \"Known user facts\" list shows what is already stored:\n\
- The user re-confirms an existing fact: output it again with the same key and a higher confidence — do not invent a new key.\n\
- A single-valued attribute (name, project_path, works_at, language, verbosity, email, city, etc.) has changed: output the latest value under the same key.\n\
- An existing fact that is unchanged and not re-confirmed: do not output it again.\n\
\n\
If no facts found, return []. Respond with ONLY the JSON array, no markdown, no explanation. NEVER extract secrets or credentials: API keys, tokens, passwords, and anything that looks like a secret must be omitted entirely.";

/// Conversation compaction summary prefix (default_model). The transcript
/// is appended after this text.
pub const CONVERSATION_SUMMARY_PROMPT: &str =
    "Summarize this conversation. Keep key facts, decisions, and context:\n\n";

/// Prefix marker of compaction summary assistant messages persisted into the
/// message stream. Shared by the compactor (which writes it), the react loop
/// (which recognizes summary messages), and the memory crate (which indexes
/// summaries as episodes) so the three cannot drift.
pub const COMPACTED_SUMMARY_PREFIX: &str = "[Compacted summary of previous messages]:";

/// LLM speech-to-text transcription (audio_model). Shared by the dedicated
/// STT client (`haven-llm`) and the media gateway's main-model fallback, so
/// the transcript prompt cannot drift between the two.
pub const STT_SYSTEM_PROMPT: &str = "You are a speech-to-text engine. Transcribe the audio verbatim in the speaker's language. Output only the transcription text, no commentary.";
pub const OCR_SYSTEM_PROMPT: &str = "You are an OCR engine. Extract all visible text from the image verbatim, preserving line breaks. Output only the extracted text, no commentary.";

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
                ("session", "test session"),
                ("skills", ""),
                ("mcps", ""),
                ("facts", ""),
                ("context", ""),
                ("history", ""),
                ("tool_notes", TOOL_USAGE_NOTES),
            ],
        );
        assert!(out.contains("You are Haven"));
        assert!(out.contains("You have access to the following built-in tools:"));
        assert!(out.contains("- read_file: read a file"));
        assert!(out.contains("Tool usage notes:"));
        assert!(out.contains("Current session: test session"));
        assert!(out.ends_with("What is your next step?\n"));
    }
}
