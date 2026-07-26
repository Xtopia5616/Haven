use std::sync::Arc;
use std::sync::RwLock;

use haven_llm::{ToolDefinition, ToolFunction};
use haven_memory::Database;
use haven_tools::ToolsManager;

use crate::types::ReActStep;

pub struct SystemPromptBuilder {
    tools: Arc<ToolsManager>,
    db: Arc<Database>,
    /// Cached serialized schema for the built-in tool list. Invalidated
    /// when the tool registry's tool count changes.
    schema_cache: RwLock<Option<SchemaCache>>,
}

#[derive(Clone)]
struct SchemaCache {
    tools_count: usize,
    built_in_section: String,
    mcp_tools_section: String,
    skill_index_section: String,
    mcp_server_index_section: String,
}

impl SystemPromptBuilder {
    pub fn new(tools: Arc<ToolsManager>, db: Arc<Database>) -> Self {
        Self {
            tools,
            db,
            schema_cache: RwLock::new(None),
        }
    }

    /// Drop any cached schema sections. Called when the tool registry
    /// changes (e.g. after register/unregister).
    pub fn invalidate_cache(&self) {
        *self.schema_cache.write().unwrap() = None;
    }

    pub async fn build(
        &self,
        task_description: &str,
        history: &[ReActStep],
        conversation_history: &[String],
    ) -> String {
        let sections = self.get_or_build_sections().await;

        let mut prompt = String::from(
            "You are Haven, a PC voice assistant. You help users accomplish tasks using available tools.\n\n\
             Available tools:\n",
        );
        prompt.push_str("You have access to the following built-in tools:\n\n");
        prompt.push_str(&sections.built_in_section);

        if !sections.mcp_tools_section.is_empty() {
            prompt.push_str("\nMCP tools (external, prefixed with `mcp::<server>::`):\n");
            prompt.push_str(&sections.mcp_tools_section);
        }

        if !sections.skill_index_section.is_empty() {
            prompt.push_str("\nInstallable skills (use `load_skill` to activate):\n");
            prompt.push_str(&sections.skill_index_section);
        }

        if !sections.mcp_server_index_section.is_empty() {
            prompt.push_str("\nAvailable MCP servers (use `load_mcp` to activate):\n");
            prompt.push_str(&sections.mcp_server_index_section);
        }

        // User facts (concise, subject = "user")
        if let Ok(facts) = self.db.get_facts("user")
            && !facts.is_empty()
        {
            prompt.push_str("\nAbout the user:");
            for fact in facts.iter().take(10) {
                prompt.push_str(&format!(
                    " [{}] {} {}",
                    if fact.source == "user" { "defined" } else { "inferred" },
                    fact.predicate,
                    fact.object,
                ));
            }
            prompt.push('\n');
        }

        // Preferences (concise)
        if let Ok(summary) = self.db.get_preference_summary()
            && !summary.is_empty()
        {
            prompt.push_str("Preferences:");
            for (key, value) in &summary {
                prompt.push_str(&format!(" {}={}", key, value));
            }
            prompt.push('\n');
        }

        prompt.push_str(
            "\nGuidelines:\n\
             1. Think step by step. Decide what to do, then call the right tool.\n\
             2. After each tool call you will receive the result. Use it to decide next.\n\
             3. When the task is complete, respond with a summary of what was done.\n\
             4. If no tool is needed, answer directly.\n\
             5. Never call the same tool with identical parameters twice in a row.\n\n",
        );

        prompt.push_str(&format!("Current task: {}\n\n", task_description));

        if !conversation_history.is_empty() {
            prompt.push_str("Additional context:\n");
            for msg in conversation_history {
                prompt.push_str(&format!("  {}\n", msg));
            }
            prompt.push('\n');
        }

        if !history.is_empty() {
            prompt.push_str("Steps so far:\n");
            for step in history {
                if let Some(ref thought) = step.thought {
                    prompt.push_str(&format!("  Thought {}: {}\n", step.step_number, thought));
                }
                if let Some(ref action) = step.action {
                    if action.is_final {
                        prompt.push_str(&format!("  Action {}: done\n", step.step_number));
                    } else {
                        prompt.push_str(&format!(
                            "  Action {}: {} {}\n",
                            step.step_number,
                            action.tool_name,
                            serde_json::to_string(&action.tool_input).unwrap_or_default()
                        ));
                    }
                }
                if let Some(ref obs) = step.observation {
                    prompt.push_str(&format!("  Result {}: {}\n", step.step_number, obs));
                }
            }
        }

        prompt.push_str("\nWhat is your next step?\n");
        prompt
    }

    pub async fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        let schemas = self.tools.registry.list_schemas().await;
        schemas
            .into_iter()
            .map(|s| ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: s["name"].as_str().unwrap_or("").into(),
                    description: s["description"].as_str().unwrap_or("").into(),
                    parameters: s["input_schema"].clone(),
                },
            })
            .collect()
    }

    async fn get_or_build_sections(&self) -> SchemaCache {
        let schemas = self.tools.registry.list_schemas().await;
        let count = schemas.len();
        {
            let cache = self.schema_cache.read().unwrap();
            if let Some(c) = cache.as_ref()
                && c.tools_count == count
            {
                return c.clone();
            }
        }

        let new_cache = self.build_sections(count, schemas).await;
        *self.schema_cache.write().unwrap() = Some(new_cache.clone());
        new_cache
    }

    async fn build_sections(&self, count: usize, schemas: Vec<serde_json::Value>) -> SchemaCache {
        let mut built_in = String::new();
        let mut mcp_tools = String::new();
        for s in &schemas {
            let name = s["name"].as_str().unwrap_or("");
            let desc = s["description"].as_str().unwrap_or("");
            if name.starts_with("mcp::") {
                mcp_tools.push_str(&format!("  - {}: {}\n", name, desc));
            } else if !name.starts_with("skill::") {
                let params = serde_json::to_string_pretty(&s["input_schema"]).unwrap_or_default();
                built_in.push_str(&format!("- {}: {}\n  {}\n", name, desc, params));
            }
        }

        let mut skill_index = String::new();
        for entry in self.tools.build_skill_index().await {
            skill_index.push_str(&format!(
                "  - {}: {}\n",
                entry["name"].as_str().unwrap_or(""),
                entry["description"].as_str().unwrap_or("")
            ));
        }

        let mut mcp_server_index = String::new();
        for entry in self.tools.build_mcp_index().await {
            mcp_server_index.push_str(&format!(
                "  - {}: {}\n",
                entry["name"].as_str().unwrap_or(""),
                entry["description"].as_str().unwrap_or("")
            ));
        }

        SchemaCache {
            tools_count: count,
            built_in_section: built_in,
            mcp_tools_section: mcp_tools,
            skill_index_section: skill_index,
            mcp_server_index_section: mcp_server_index,
        }
    }
}