use haven_common::types::RiskLevel;
use serde_json::Value;

/// Expected operation/risk rows used by the local-tool security regression
/// matrix. The registry-driven test below verifies that this contract covers
/// every operation actually advertised by every builtin tool, so schema and
/// risk-level changes cannot silently leave the matrix stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalToolSecurityCase {
    pub(crate) tool_name: &'static str,
    pub(crate) operation: &'static str,
    pub(crate) risk_level: RiskLevel,
}

macro_rules! security_case {
    ($tool:literal, $operation:literal, $risk:ident) => {
        LocalToolSecurityCase {
            tool_name: $tool,
            operation: $operation,
            risk_level: RiskLevel::$risk,
        }
    };
}

pub(crate) const LOCAL_TOOL_SECURITY_MATRIX: &[LocalToolSecurityCase] = &[
    security_case!("ask", "ask", Safe),
    security_case!("files.read", "files.read", Low),
    security_case!("files.inspect", "files.inspect", Low),
    security_case!("files.stat", "files.stat", Low),
    security_case!("files.hash", "files.hash", Low),
    security_case!("files.outline", "files.outline", Low),
    security_case!("files.summary", "files.summary", Low),
    security_case!("files.search", "files.search", Low),
    security_case!("files.search", "search:content", Medium),
    security_case!("files.write", "files.write", Medium),
    security_case!("files.create_dir", "files.create_dir", Medium),
    security_case!("files.edit", "files.edit", Medium),
    security_case!("files.patch", "files.patch", Medium),
    security_case!("files.copy", "files.copy", Medium),
    security_case!("files.move", "files.move", Medium),
    security_case!("files.delete", "files.delete", High),
    security_case!("files.list", "files.list", Low),
    security_case!("lsp.definition", "lsp.definition", High),
    security_case!("lsp.implementation", "lsp.implementation", High),
    security_case!("lsp.type_definition", "lsp.type_definition", High),
    security_case!("lsp.references", "lsp.references", High),
    security_case!("lsp.hover", "lsp.hover", High),
    security_case!("lsp.document_symbols", "lsp.document_symbols", High),
    security_case!("lsp.workspace_symbols", "lsp.workspace_symbols", High),
    security_case!("lsp.diagnostics", "lsp.diagnostics", High),
    security_case!("clipboard.read", "clipboard.read", Low),
    security_case!("clipboard.write", "clipboard.write", Medium),
    security_case!("clipboard.history", "clipboard.history", Low),
    security_case!("shell", "execute", High),
    security_case!("toolrun.start", "toolrun.start", Safe),
    security_case!("toolrun.list", "toolrun.list", Safe),
    security_case!("toolrun.inspect", "toolrun.inspect", Safe),
    security_case!("toolrun.cancel", "toolrun.cancel", Medium),
    security_case!("input.type", "input.type", Medium),
    security_case!("input.type_element", "input.type_element", Medium),
    security_case!("input.key", "input.key", Medium),
    security_case!("input.click", "input.click", Medium),
    security_case!("input.click_element", "input.click_element", Medium),
    security_case!("input.move", "input.move", Low),
    security_case!("input.scroll", "input.scroll", Low),
    security_case!("system.info", "system.info", Safe),
    security_case!("system.display", "system.display", Safe),
    security_case!("system.env.list", "system.env.list", High),
    security_case!("system.env.get", "system.env.get", Low),
    security_case!("system.registry.list", "system.registry.list", Medium),
    security_case!("system.registry.get", "system.registry.get", Medium),
    security_case!("system.power.status", "system.power.status", Safe),
    security_case!("system.power.lock", "system.power.lock", High),
    security_case!("system.power.sleep", "system.power.sleep", High),
    security_case!("system.power.hibernate", "system.power.hibernate", Critical),
    security_case!("schedule.set", "schedule.set", Low),
    security_case!("schedule.list", "schedule.list", Safe),
    security_case!("schedule.cancel", "schedule.cancel", Safe),
    security_case!("window.list", "window.list", Low),
    security_case!("window.foreground", "window.foreground", Low),
    security_case!("window.focus", "window.focus", Medium),
    security_case!("window.close", "window.close", High),
    security_case!("window.screenshot", "window.screenshot", Low),
    security_case!("window.ui_tree", "window.ui_tree", Low),
    security_case!("window.observe", "window.observe", Low),
    security_case!("window.invoke", "window.invoke", Medium),
    security_case!("window.set_value", "window.set_value", Medium),
    security_case!("window.toggle", "window.toggle", Medium),
    security_case!("window.select", "window.select", Medium),
    security_case!("window.wait", "window.wait", Low),
    security_case!("media.inspect", "media.inspect", Low),
    security_case!("media.describe", "media.describe", Medium),
    security_case!("media.ocr", "media.ocr", High),
    security_case!("media.transcribe", "media.transcribe", Medium),
    security_case!("media.extract", "media.extract", Low),
    security_case!("media.render", "media.render", Low),
    security_case!("media.generate", "media.generate", Medium),
    security_case!("media.record", "media.record", Medium),
    security_case!("media.play", "media.play", Low),
    security_case!("media.speak", "media.speak", Low),
    security_case!("media.volume_get", "media.volume_get", Low),
    security_case!("media.volume_set", "media.volume_set", Medium),
    security_case!("media.mute_get", "media.mute_get", Low),
    security_case!("media.mute_set", "media.mute_set", Medium),
    security_case!("http", "request", Medium),
    security_case!("notify", "notify", Safe),
    security_case!("tool_catalog", "tool_catalog", Safe),
    security_case!("load_skill", "load", Safe),
    security_case!("agent.list", "agent.list", Safe),
    security_case!("agent.children", "agent.children", Safe),
    security_case!("agent.history", "agent.history", Safe),
    security_case!("agent.send", "agent.send", Safe),
    security_case!("agent.inbox", "agent.inbox", Safe),
    security_case!("agent.ack", "agent.ack", Safe),
    security_case!("agent.reply", "agent.reply", Safe),
    security_case!("agent.profile", "agent.profile", Safe),
    security_case!("agent.request", "agent.request", Safe),
    security_case!("session.create", "session.create", Medium),
    security_case!("session.link", "session.link", Low),
    security_case!("session.unlink", "session.unlink", Low),
    security_case!("session.links", "session.links", Safe),
    security_case!("session.read_notes", "session.read_notes", Safe),
    security_case!("session.replace_notes", "session.replace_notes", Safe),
    security_case!("agent.status", "agent.status", Safe),
    security_case!("agent.wait", "agent.wait", Safe),
    security_case!("agent.stop", "agent.stop", High),
    security_case!("agent.collect", "agent.collect", Safe),
    security_case!("load_mcp", "load", Safe),
    security_case!("memory.search", "memory.search", Safe),
    security_case!("memory.list", "memory.list", Safe),
    security_case!("memory.remember", "memory.remember", Medium),
    security_case!("memory.forget", "memory.forget", Medium),
    security_case!("memory.recall", "memory.recall", Safe),
    security_case!("haven.diagnostics.status", "haven.diagnostics.status", Low),
    security_case!(
        "haven.diagnostics.logs_tail",
        "haven.diagnostics.logs_tail",
        Low
    ),
    security_case!(
        "haven.diagnostics.sessions",
        "haven.diagnostics.sessions",
        Low
    ),
    security_case!("haven.diagnostics.errors", "haven.diagnostics.errors", Low),
    security_case!("haven.config.config_get", "haven.config.config_get", Low),
    security_case!("haven.config.logs_level", "haven.config.logs_level", Medium),
    security_case!("haven.skills.skills_list", "haven.skills.skills_list", Low),
    security_case!(
        "haven.skills.skill_enable",
        "haven.skills.skill_enable",
        Medium
    ),
    security_case!(
        "haven.skills.skill_disable",
        "haven.skills.skill_disable",
        Medium
    ),
    security_case!(
        "haven.skills.skill_create",
        "haven.skills.skill_create",
        High
    ),
    security_case!("haven.tools.tool_enable", "haven.tools.tool_enable", Medium),
    security_case!(
        "haven.tools.tool_disable",
        "haven.tools.tool_disable",
        Medium
    ),
    security_case!("haven.mcp.mcp_list", "haven.mcp.mcp_list", Low),
    security_case!("haven.mcp.mcp_connect", "haven.mcp.mcp_connect", Medium),
    security_case!(
        "haven.mcp.mcp_disconnect",
        "haven.mcp.mcp_disconnect",
        Medium
    ),
    security_case!("haven.mcp.mcp_add", "haven.mcp.mcp_add", High),
    security_case!("haven.mcp.mcp_update", "haven.mcp.mcp_update", High),
    security_case!("haven.mcp.mcp_toggle", "haven.mcp.mcp_toggle", High),
    security_case!("haven.mcp.mcp_remove", "haven.mcp.mcp_remove", High),
    security_case!("haven.mcp.mcp_reload", "haven.mcp.mcp_reload", High),
];

/// Build a concise, non-sensitive explanation for a permission prompt.
///
/// Raw tool arguments are intentionally kept inside the backend confirmation
/// state. They can contain shell commands, URLs with credentials, file
/// contents, or MCP/skill secrets and must never be sent to the renderer.
pub fn permission_prompt_summary(tool_name: &str, params: &Value) -> String {
    if tool_name == "open_external" {
        let target = params
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if target
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
            || target
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        {
            return "将在系统浏览器中打开外部链接（链接详情已隐藏）".into();
        }
        return "将在文件管理器中打开本地路径（路径详情已隐藏）".into();
    }
    if tool_name == "open_skills_dir" {
        return "将在文件管理器中打开配置的 Skills 目录（路径已隐藏）".into();
    }
    if tool_name == "files.promote_sandbox_output" {
        let change_index = params
            .get("change_index")
            .and_then(Value::as_u64)
            .map(|index| format!("#{index}"))
            .unwrap_or_else(|| "所选文件".into());
        let destination = params
            .get("destination")
            .and_then(Value::as_str)
            .map(|path| {
                path.chars()
                    .filter(|character| !character.is_control())
                    .take(240)
                    .collect::<String>()
            })
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "所选目标".into());
        return format!(
            "将审阅结果 {change_index} 创建到：{destination}。目标已存在时会拒绝覆盖。"
        );
    }
    if tool_name == "haven.mcp.mcp_reconnect" {
        return "将重新连接一个当前配置的 MCP 服务器（服务器配置详情已隐藏）".into();
    }
    if tool_name == "haven.mcp.mcp_refresh" {
        let mut connect = 0;
        let mut reconnect = 0;
        let mut disconnect = 0;
        if let Some(targets) = params.pointer("/plan/targets").and_then(Value::as_array) {
            for target in targets {
                match target.get("action").and_then(Value::as_str) {
                    Some("connect") => connect += 1,
                    Some("reconnect") => reconnect += 1,
                    Some("disconnect") => disconnect += 1,
                    _ => {}
                }
            }
        }
        if connect + reconnect + disconnect == 0 {
            return "MCP 配置刷新：当前无需连接或断开服务器".into();
        }
        return format!(
            "MCP 配置变更批次：连接 {connect} 台、重连 {reconnect} 台、断开 {disconnect} 台服务器（配置详情已隐藏）"
        );
    }
    let operation = registered_operation_label(tool_name, params);
    let family = tool_name
        .split(':')
        .next()
        .and_then(|name| name.split('.').next())
        .unwrap_or(tool_name);
    match family {
        "files" => format!("文件操作：{operation}（目标详情已隐藏）"),
        "shell" => "将执行一条受保护的本机命令（命令内容不会显示在弹窗中）".into(),
        "http" => "将向外部网络发起请求（请求内容已隐藏）".into(),
        "media" => format!("媒体操作：{operation}（参数详情已隐藏）"),
        "system" => format!("系统操作：{operation}（参数详情已隐藏）"),
        "haven" => format!("Haven 操作：{operation}（参数详情已隐藏）"),
        "mcp" | "skill" => format!("扩展能力将执行：{tool_name}（参数已隐藏）"),
        _ => format!("工具 {tool_name} 将执行受保护操作：{operation}"),
    }
}

/// Return an operation only when it is present in the backend security
/// registry. Dynamic extension arguments must never become renderer text;
/// unknown values collapse to a generic label.
fn registered_operation_label(tool_name: &str, params: &Value) -> String {
    if tool_name == "files.search" && params["mode"].as_str() == Some("content") {
        return "search:content".into();
    }
    let operation = params.get("operation").and_then(Value::as_str);
    let scope = params.get("scope").and_then(Value::as_str);
    let candidate = match (scope, operation) {
        (Some(scope), Some(operation)) if !scope.is_empty() && !operation.is_empty() => {
            format!("{scope}:{operation}")
        }
        (None, Some(operation)) if !operation.is_empty() => operation.to_string(),
        (Some(scope), None) if !scope.is_empty() => scope.to_string(),
        _ => String::new(),
    };
    if !candidate.is_empty()
        && LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .any(|case| case.tool_name == tool_name && case.operation == candidate)
    {
        return candidate;
    }
    // Operation views use their public name as the permission-matrix
    // operation while their canonical execution input carries the aggregate
    // tool's discriminator (for example `read`).
    if LOCAL_TOOL_SECURITY_MATRIX
        .iter()
        .any(|case| case.tool_name == tool_name && case.operation == tool_name)
    {
        return tool_name.to_string();
    }
    if operation.is_some_and(|operation| {
        LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .any(|case| case.tool_name == tool_name && case.operation == operation)
    }) {
        return operation.unwrap_or_default().to_string();
    }
    "受保护操作".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn permission_prompt_summary_never_includes_raw_sensitive_arguments() {
        let summary = permission_prompt_summary(
            "shell",
            &json!({"command": "curl https://example.test?token=super-secret"}),
        );
        assert!(summary.contains("受保护的本机命令"));
        assert!(!summary.contains("super-secret"));
        assert!(!summary.contains("curl"));
    }

    #[test]
    fn sandbox_output_promotion_prompt_shows_bounded_destination_and_no_overwrite_rule() {
        let summary = permission_prompt_summary(
            "files.promote_sandbox_output",
            &json!({
                "change_index": 2,
                "destination": "C:/Users/example/Workspace/result.txt\nignored",
                "private_source_path": "C:/private/sandbox/file.txt"
            }),
        );
        assert!(summary.contains("#2"));
        assert!(summary.contains("C:/Users/example/Workspace/result.txtignored"));
        assert!(summary.contains("拒绝覆盖"));
        assert!(!summary.contains("private_source_path"));
        assert!(!summary.contains("C:/private/sandbox/file.txt"));
    }

    #[test]
    fn permission_prompt_summary_uses_generic_text_for_unknown_operations() {
        let summary = permission_prompt_summary(
            "files",
            &json!({"operation": "custom-secret-operation", "path": "C:/private.txt"}),
        );
        assert!(summary.contains("受保护操作"));
        assert!(!summary.contains("custom-secret-operation"));
        assert!(!summary.contains("private.txt"));
    }

    #[test]
    fn permission_prompt_summary_routes_operation_views_to_their_family() {
        let summary = permission_prompt_summary(
            "files.read",
            &json!({"operation": "read", "path": "private.txt"}),
        );
        assert!(summary.starts_with("文件操作："));
        assert!(summary.contains("files.read"));
        assert!(!summary.contains("private.txt"));
    }

    #[test]
    fn permission_prompt_summary_routes_media_operations_without_arguments() {
        let summary = permission_prompt_summary(
            "media.speak",
            &json!({"operation": "speak", "text": "secret spoken content"}),
        );
        assert!(summary.starts_with("媒体操作：media.speak"));
        assert!(!summary.contains("secret spoken content"));
    }

    #[test]
    fn native_mcp_refresh_prompt_describes_one_bounded_batch_without_config_values() {
        let summary = permission_prompt_summary(
            "haven.mcp.mcp_refresh",
            &serde_json::json!({
                "operation": "mcp_refresh",
                "plan": {
                    "config_version": 4,
                    "targets": [
                        {"name": "alpha", "action": "connect"},
                        {"name": "beta", "action": "reconnect"},
                        {"name": "gamma", "action": "disconnect"}
                    ]
                },
                "command": "secret command",
                "url": "https://secret.invalid"
            }),
        );
        assert_eq!(
            summary,
            "MCP 配置变更批次：连接 1 台、重连 1 台、断开 1 台服务器（配置详情已隐藏）"
        );
        assert!(!summary.contains("alpha"));
        assert!(!summary.contains("secret"));
    }

    #[test]
    fn native_mcp_reconnect_prompt_names_its_single_server_scope() {
        let summary = permission_prompt_summary(
            "haven.mcp.mcp_reconnect",
            &serde_json::json!({"operation": "mcp_reconnect", "name": "private-server"}),
        );
        assert!(summary.contains("一个"));
        assert!(!summary.contains("private-server"));
    }
}
