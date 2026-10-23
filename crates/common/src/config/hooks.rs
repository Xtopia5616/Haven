use serde::{Deserialize, Serialize};

/// External commands configured for Haven lifecycle events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct LifecycleHooksConfig {
    pub pre_tool_use: Vec<HookCommand>,
    pub post_tool_use: Vec<HookCommand>,
    pub session_start: Vec<HookCommand>,
    pub run_start: Vec<HookCommand>,
    pub run_end: Vec<HookCommand>,
    pub pre_compact: Vec<HookCommand>,
    pub post_compact: Vec<HookCommand>,
}

impl LifecycleHooksConfig {
    pub fn for_event(&self, event: crate::hooks::LifecycleHookEvent) -> &[HookCommand] {
        match event {
            crate::hooks::LifecycleHookEvent::PreToolUse => &self.pre_tool_use,
            crate::hooks::LifecycleHookEvent::PostToolUse => &self.post_tool_use,
            crate::hooks::LifecycleHookEvent::SessionStart => &self.session_start,
            crate::hooks::LifecycleHookEvent::RunStart => &self.run_start,
            crate::hooks::LifecycleHookEvent::RunEnd => &self.run_end,
            crate::hooks::LifecycleHookEvent::PreCompact => &self.pre_compact,
            crate::hooks::LifecycleHookEvent::PostCompact => &self.post_compact,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pre_tool_use.is_empty()
            && self.post_tool_use.is_empty()
            && self.session_start.is_empty()
            && self.run_start.is_empty()
            && self.run_end.is_empty()
            && self.pre_compact.is_empty()
            && self.post_compact.is_empty()
    }
}

/// One executable and its literal arguments. No shell expansion or string
/// interpolation is performed. A missing matcher applies to every tool.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookCommand {
    /// Disabled commands remain validated and ordered, but never execute.
    #[serde(default = "hook_enabled_by_default")]
    pub enabled: bool,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub matcher: Option<String>,
}

fn hook_enabled_by_default() -> bool {
    true
}

impl std::fmt::Debug for HookCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookCommand")
            .field("enabled", &self.enabled)
            .field("command", &"<redacted>")
            .field(
                "args",
                &format_args!("<redacted: {} args>", self.args.len()),
            )
            .field("matcher", &self.matcher)
            .finish()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectHooksFile {
    #[serde(default)]
    hooks: LifecycleHooksConfig,
}

/// Parse the project-local `.haven/hooks.toml` shape and validate it before
/// any configured process can start.
pub fn parse_project_hooks(input: &str) -> anyhow::Result<LifecycleHooksConfig> {
    let file: ProjectHooksFile = toml::from_str(input)?;
    validate_lifecycle_hooks(&file.hooks)?;
    Ok(file.hooks)
}

pub(crate) fn validate_lifecycle_hooks(config: &LifecycleHooksConfig) -> anyhow::Result<()> {
    for (event, commands) in [
        ("pre_tool_use", &config.pre_tool_use),
        ("post_tool_use", &config.post_tool_use),
        ("session_start", &config.session_start),
        ("run_start", &config.run_start),
        ("run_end", &config.run_end),
        ("pre_compact", &config.pre_compact),
        ("post_compact", &config.post_compact),
    ] {
        anyhow::ensure!(
            commands.len() <= super::MAX_HOOKS_PER_EVENT,
            "hooks.{event} must not contain more than {} commands",
            super::MAX_HOOKS_PER_EVENT
        );
        for (index, hook) in commands.iter().enumerate() {
            anyhow::ensure!(
                !hook.command.trim().is_empty()
                    && hook.command.len() <= super::MAX_HOOK_COMMAND_BYTES
                    && !hook.command.chars().any(char::is_control),
                "hooks.{event}[{index}].command must be a non-empty executable path within the size limit"
            );
            anyhow::ensure!(
                hook.args.len() <= super::MAX_HOOK_ARGS,
                "hooks.{event}[{index}].args must not contain more than {} arguments",
                super::MAX_HOOK_ARGS
            );
            anyhow::ensure!(
                hook.args.iter().all(|arg| {
                    arg.len() <= super::MAX_HOOK_ARG_BYTES && !arg.chars().any(char::is_control)
                }),
                "hooks.{event}[{index}].args contain an invalid or oversized argument"
            );
            if let Some(matcher) = hook.matcher.as_deref() {
                anyhow::ensure!(
                    !matcher.is_empty()
                        && matcher.len() <= super::MAX_HOOK_MATCHER_BYTES
                        && !matcher
                            .chars()
                            .any(|ch| ch.is_control() || ch.is_whitespace()),
                    "hooks.{event}[{index}].matcher must be a tool name pattern with '*' wildcards"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_hooks_parse_from_the_named_hooks_section() {
        let hooks = parse_project_hooks(
            r#"
                [hooks]
                [[hooks.pre_tool_use]]
                command = "pwsh.exe"
                args = ["-NoProfile", "-File", "scripts/pre-tool.ps1"]
                matcher = "shell"
            "#,
        )
        .unwrap();

        assert_eq!(hooks.pre_tool_use.len(), 1);
        assert_eq!(hooks.pre_tool_use[0].command, "pwsh.exe");
        assert!(hooks.pre_tool_use[0].enabled);
        assert_eq!(hooks.pre_tool_use[0].matcher.as_deref(), Some("shell"));
    }

    #[test]
    fn disabled_hooks_round_trip_and_still_require_valid_commands() {
        let hooks =
            parse_project_hooks("[[hooks.run_start]]\ncommand = 'hook.exe'\nenabled = false")
                .unwrap();
        assert!(!hooks.run_start[0].enabled);
        let encoded = toml::to_string(&ProjectHooksFileForTest { hooks: &hooks }).unwrap();
        assert_eq!(parse_project_hooks(&encoded).unwrap(), hooks);
        assert!(parse_project_hooks("[[hooks.run_start]]\ncommand = ''\nenabled = false").is_err());
        assert!(
            parse_project_hooks("[[hooks.run_start]]\ncommand = 'hook.exe'\nenabled = 'false'")
                .is_err()
        );
    }

    #[derive(Serialize)]
    struct ProjectHooksFileForTest<'a> {
        hooks: &'a LifecycleHooksConfig,
    }

    #[test]
    fn project_hooks_accept_bounded_tool_globs_and_reject_control_characters() {
        let glob_matcher = LifecycleHooksConfig {
            pre_tool_use: vec![HookCommand {
                enabled: true,
                command: "hook.exe".into(),
                args: Vec::new(),
                matcher: Some("shell.*".into()),
            }],
            ..Default::default()
        };
        assert!(validate_lifecycle_hooks(&glob_matcher).is_ok());

        let invalid_matcher = LifecycleHooksConfig {
            pre_tool_use: vec![HookCommand {
                enabled: true,
                command: "hook.exe".into(),
                args: Vec::new(),
                matcher: Some("shell.\n*".into()),
            }],
            ..Default::default()
        };
        assert!(validate_lifecycle_hooks(&invalid_matcher).is_err());

        let invalid_command = LifecycleHooksConfig {
            pre_tool_use: vec![HookCommand {
                enabled: true,
                command: "hook.exe\n--unsafe".into(),
                args: Vec::new(),
                matcher: None,
            }],
            ..Default::default()
        };
        assert!(validate_lifecycle_hooks(&invalid_command).is_err());
    }

    #[test]
    fn hook_debug_output_redacts_executable_and_arguments() {
        let hook = HookCommand {
            enabled: true,
            command: "C:/private/secret.exe".into(),
            args: vec!["--token=secret".into()],
            matcher: Some("shell".into()),
        };

        let debug = format!("{hook:?}");

        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }
}
