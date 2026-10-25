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
    security_case!("files.get_outline", "files.get_outline", Low),
    security_case!("files.summarize", "files.summarize", Low),
    security_case!("files.search", "files.search", Low),
    security_case!("files.search", "search:content", Medium),
    security_case!("files.write", "files.write", Medium),
    security_case!("files.create_directory", "files.create_directory", Medium),
    security_case!("files.patch", "files.patch", Medium),
    security_case!("files.copy", "files.copy", Medium),
    security_case!("files.move", "files.move", Medium),
    security_case!("files.delete", "files.delete", High),
    security_case!("files.list", "files.list", Low),
    security_case!("lsp.find_definition", "lsp.find_definition", High),
    security_case!("lsp.find_implementations", "lsp.find_implementations", High),
    security_case!("lsp.find_type_definition", "lsp.find_type_definition", High),
    security_case!("lsp.find_references", "lsp.find_references", High),
    security_case!("lsp.get_hover", "lsp.get_hover", High),
    security_case!(
        "lsp.list_document_symbols",
        "lsp.list_document_symbols",
        High
    ),
    security_case!(
        "lsp.search_workspace_symbols",
        "lsp.search_workspace_symbols",
        High
    ),
    security_case!("lsp.get_diagnostics", "lsp.get_diagnostics", High),
    security_case!("clipboard.read", "clipboard.read", Low),
    security_case!("clipboard.write", "clipboard.write", Medium),
    security_case!("clipboard.list_history", "clipboard.list_history", Low),
    security_case!("shell", "execute", High),
    security_case!("git", "git", High),
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
    security_case!("system.get_info", "system.get_info", Safe),
    security_case!("system.list_displays", "system.list_displays", Safe),
    security_case!("system.env.list", "system.env.list", High),
    security_case!("system.env.get", "system.env.get", Low),
    security_case!("system.registry.list", "system.registry.list", Medium),
    security_case!("system.registry.get", "system.registry.get", Medium),
    security_case!("system.power.get_status", "system.power.get_status", Safe),
    security_case!("system.power.lock", "system.power.lock", High),
    security_case!("system.power.sleep", "system.power.sleep", High),
    security_case!("system.power.hibernate", "system.power.hibernate", Critical),
    security_case!("schedule.set", "schedule.set", Low),
    security_case!("schedule.list", "schedule.list", Safe),
    security_case!("schedule.cancel", "schedule.cancel", Safe),
    security_case!("window.list", "window.list", Low),
    security_case!("window.get_foreground", "window.get_foreground", Low),
    security_case!("window.focus", "window.focus", Medium),
    security_case!("window.close", "window.close", High),
    security_case!(
        "window.capture_screenshot",
        "window.capture_screenshot",
        Low
    ),
    security_case!("window.get_ui_tree", "window.get_ui_tree", Low),
    security_case!("window.observe", "window.observe", Low),
    security_case!("window.invoke", "window.invoke", Medium),
    security_case!("window.set_value", "window.set_value", Medium),
    security_case!("window.toggle", "window.toggle", Medium),
    security_case!("window.select", "window.select", Medium),
    security_case!("window.wait", "window.wait", Low),
    security_case!("media.inspect", "media.inspect", Low),
    security_case!("media.describe", "media.describe", Medium),
    security_case!("media.run_ocr", "media.run_ocr", High),
    security_case!("media.transcribe", "media.transcribe", Medium),
    security_case!("media.extract", "media.extract", Low),
    security_case!("media.render", "media.render", Low),
    security_case!("media.generate", "media.generate", Medium),
    security_case!("media.record", "media.record", Medium),
    security_case!("media.play", "media.play", Low),
    security_case!("media.speak", "media.speak", Low),
    security_case!("media.get_volume", "media.get_volume", Low),
    security_case!("media.set_volume", "media.set_volume", Medium),
    security_case!("media.get_mute", "media.get_mute", Low),
    security_case!("media.set_mute", "media.set_mute", Medium),
    security_case!("http", "request", Medium),
    security_case!("notify", "notify", Safe),
    security_case!("discover_tools", "discover_tools", Safe),
    security_case!("load_skill", "load", Safe),
    security_case!("agent.list", "agent.list", Safe),
    security_case!("agent.list_children", "agent.list_children", Safe),
    security_case!("agent.read_history", "agent.read_history", Safe),
    security_case!("agent.send", "agent.send", Safe),
    security_case!("agent.list_inbox", "agent.list_inbox", Safe),
    security_case!("agent.acknowledge", "agent.acknowledge", Safe),
    security_case!("agent.reply", "agent.reply", Safe),
    security_case!("agent.get_profile", "agent.get_profile", Safe),
    security_case!("agent.request", "agent.request", Safe),
    security_case!("session.create", "session.create", Medium),
    security_case!("session.link", "session.link", Low),
    security_case!("session.unlink", "session.unlink", Low),
    security_case!("session.list_links", "session.list_links", Safe),
    security_case!("session.read_notes", "session.read_notes", Safe),
    security_case!("session.replace_notes", "session.replace_notes", Safe),
    security_case!("session.search_history", "session.search_history", Safe),
    security_case!("session.read_history", "session.read_history", Safe),
    security_case!(
        "session.search_linked_history",
        "session.search_linked_history",
        Safe
    ),
    security_case!(
        "session.read_linked_history",
        "session.read_linked_history",
        Safe
    ),
    security_case!("session.search_assets", "session.search_assets", Safe),
    security_case!("agent.get_status", "agent.get_status", Safe),
    security_case!("agent.wait", "agent.wait", Safe),
    security_case!("agent.stop", "agent.stop", High),
    security_case!("agent.collect", "agent.collect", Safe),
    security_case!("load_mcp", "load", Safe),
    security_case!("memory.search", "memory.search", Safe),
    security_case!("memory.list", "memory.list", Safe),
    security_case!("memory.remember", "memory.remember", Medium),
    security_case!("memory.forget", "memory.forget", Medium),
    security_case!("memory.recall", "memory.recall", Safe),
    security_case!(
        "haven.diagnostics.get_status",
        "haven.diagnostics.get_status",
        Low
    ),
    security_case!(
        "haven.diagnostics.tail_logs",
        "haven.diagnostics.tail_logs",
        Low
    ),
    security_case!(
        "haven.diagnostics.list_sessions",
        "haven.diagnostics.list_sessions",
        Low
    ),
    security_case!(
        "haven.diagnostics.list_errors",
        "haven.diagnostics.list_errors",
        Low
    ),
    security_case!("haven.config.get", "haven.config.get", Low),
    security_case!(
        "haven.config.set_log_level",
        "haven.config.set_log_level",
        Medium
    ),
    security_case!("haven.skills.list", "haven.skills.list", Low),
    security_case!("haven.skills.enable", "haven.skills.enable", Medium),
    security_case!("haven.skills.disable", "haven.skills.disable", Medium),
    security_case!("haven.skills.create", "haven.skills.create", High),
    security_case!("haven.tools.enable", "haven.tools.enable", Medium),
    security_case!("haven.tools.disable", "haven.tools.disable", Medium),
    security_case!("haven.mcp.list", "haven.mcp.list", Low),
    security_case!("haven.mcp.connect", "haven.mcp.connect", Medium),
    security_case!("haven.mcp.disconnect", "haven.mcp.disconnect", Medium),
    security_case!("haven.mcp.add", "haven.mcp.add", High),
    security_case!("haven.mcp.update", "haven.mcp.update", High),
    security_case!("haven.mcp.toggle", "haven.mcp.toggle", High),
    security_case!("haven.mcp.remove", "haven.mcp.remove", High),
    security_case!("haven.mcp.reload", "haven.mcp.reload", High),
];

/// Build a concise summary that avoids repeating the invocation arguments.
/// The confirmation event carries those arguments separately for the UI's
/// structured detail view, which folds values that look like credentials.
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
            return "将在系统浏览器中打开外部链接；请检查下方调用参数。".into();
        }
        return "将在文件管理器中打开本地路径；请检查下方调用参数。".into();
    }
    if tool_name == "open_skills_dir" {
        return "将在文件管理器中打开配置的 Skills 目录；请检查下方调用参数。".into();
    }
    if tool_name == "haven.mcp.reconnect" {
        return "将重新连接一个当前配置的 MCP 服务器；请检查下方调用参数。".into();
    }
    if tool_name == "haven.mcp.refresh" {
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
            "MCP 配置变更批次：连接 {connect} 台、重连 {reconnect} 台、断开 {disconnect} 台服务器；请检查下方调用参数。"
        );
    }
    let operation = registered_operation_label(tool_name, params);
    let family = tool_name
        .split(':')
        .next()
        .and_then(|name| name.split('.').next())
        .unwrap_or(tool_name);
    match family {
        "files" => format!("文件操作：{operation}；请检查下方调用参数。"),
        "shell" => "将执行一条受保护的本机命令；请检查下方调用参数。".into(),
        "http" => "将向外部网络发起请求；请检查下方调用参数。".into(),
        "web_search_api" if params["fetch_top_results"].as_u64().unwrap_or_default() > 0 => {
            format!(
                "将联网搜索，并读取最多 {} 个公开来源网页；请检查下方调用参数。",
                params["fetch_top_results"].as_u64().unwrap_or_default()
            )
        }
        "web_search_api" => "将使用已配置的联网搜索服务；请检查下方调用参数。".into(),
        "media" => format!("媒体操作：{operation}；请检查下方调用参数。"),
        "system" => format!("系统操作：{operation}；请检查下方调用参数。"),
        "haven" => format!("Haven 操作：{operation}；请检查下方调用参数。"),
        "mcp" | "skill" => format!("扩展能力将执行：{tool_name}；请检查下方调用参数。"),
        _ => format!("工具 {tool_name} 将执行受保护操作：{operation}"),
    }
}

/// Return an operation label only when it is present in the backend security
/// registry. Unknown values collapse to a generic summary label; the concrete
/// arguments remain visible in the structured confirmation details.
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
            "haven.mcp.refresh",
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
            "MCP 配置变更批次：连接 1 台、重连 1 台、断开 1 台服务器；请检查下方调用参数。"
        );
        assert!(!summary.contains("alpha"));
        assert!(!summary.contains("secret"));
    }

    #[test]
    fn native_mcp_reconnect_prompt_names_its_single_server_scope() {
        let summary = permission_prompt_summary(
            "haven.mcp.reconnect",
            &serde_json::json!({"operation": "mcp_reconnect", "name": "private-server"}),
        );
        assert!(summary.contains("一个"));
        assert!(!summary.contains("private-server"));
    }
}
