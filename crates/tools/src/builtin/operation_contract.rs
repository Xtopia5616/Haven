//! Canonical metadata for builtin operation views.
//!
//! The aggregate implementations own execution. This module owns the stable
//! operation identity metadata shared by the operation-view builder, policy
//! projection, prompt catalog and UI manifest. Schemas remain close to the
//! implementation because they can depend on runtime limits.

use crate::{NetworkAccess, OperationIdempotency};
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationContract {
    pub(crate) name: &'static str,
    pub(crate) label: &'static str,
    pub(crate) catalog_group: ToolCatalogGroup,
    /// Describes the operation's effect, independently of its scheduling
    /// concurrency. Sensitive data and network access can still require
    /// confirmation for a read-only operation.
    pub(crate) read_only: bool,
    /// Explicit network classification for operations that need a non-default
    /// boundary from the generic name-based inference.
    pub(crate) network_access_override: Option<NetworkAccess>,
    pub(crate) risk_override: Option<RiskLevel>,
    pub(crate) idempotency: OperationIdempotency,
}

/// Return the single stable metadata record for a model-facing builtin
/// operation. Unknown names stay readable and conservative, which keeps a
/// newly added operation visible while forcing an explicit contract entry for
/// read-only or elevated-risk behavior.
pub(crate) fn operation_contract(name: &'static str) -> OperationContract {
    let (label, catalog_group, read_only, risk_override) = match name {
        "files.read" => ("读取文件", ToolCatalogGroup::Files, true, None),
        "files.get_outline" => ("文件大纲", ToolCatalogGroup::Files, true, None),
        "files.summarize" => ("文件摘要", ToolCatalogGroup::Files, true, None),
        "files.search" => ("搜索文件", ToolCatalogGroup::Files, true, None),
        "files.inspect" => ("检查文件元数据", ToolCatalogGroup::Files, true, None),
        "files.write" => ("写入文件", ToolCatalogGroup::Files, false, None),
        "files.create_directory" => ("创建目录", ToolCatalogGroup::Files, false, None),
        "files.patch" => ("精确修改文件", ToolCatalogGroup::Files, false, None),
        "files.copy" => ("复制文件", ToolCatalogGroup::Files, false, None),
        "files.move" => ("移动文件", ToolCatalogGroup::Files, false, None),
        "files.delete" => ("删除文件", ToolCatalogGroup::Files, false, None),
        "files.list" => ("列出文件", ToolCatalogGroup::Files, true, None),
        "lsp.find_definition" => (
            "代码定义",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.find_implementations" => (
            "代码实现",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.find_type_definition" => (
            "类型定义",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.find_references" => (
            "代码引用",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.get_hover" => (
            "代码悬停信息",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.list_document_symbols" => (
            "文档符号",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.search_workspace_symbols" => (
            "工作区符号搜索",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "lsp.get_diagnostics" => (
            "代码诊断",
            ToolCatalogGroup::Files,
            false,
            Some(RiskLevel::High),
        ),
        "clipboard.read" => ("读取剪贴板", ToolCatalogGroup::Desktop, true, None),
        "clipboard.write" => ("写入剪贴板", ToolCatalogGroup::Desktop, false, None),
        "clipboard.list_history" => ("剪贴板历史", ToolCatalogGroup::Desktop, true, None),
        "input.type" => ("输入文字", ToolCatalogGroup::Desktop, false, None),
        "input.type_element" => ("向控件输入文字", ToolCatalogGroup::Desktop, false, None),
        "input.key" => ("按键", ToolCatalogGroup::Desktop, false, None),
        "input.click" => ("点击", ToolCatalogGroup::Desktop, false, None),
        "input.click_element" => ("点击控件", ToolCatalogGroup::Desktop, false, None),
        "input.move" => ("移动鼠标", ToolCatalogGroup::Desktop, false, None),
        "input.scroll" => ("滚动", ToolCatalogGroup::Desktop, false, None),
        "window.list" => ("列出窗口", ToolCatalogGroup::Desktop, true, None),
        "window.get_foreground" => ("前台窗口", ToolCatalogGroup::Desktop, true, None),
        "window.focus" => ("聚焦窗口", ToolCatalogGroup::Desktop, false, None),
        "window.close" => ("关闭窗口", ToolCatalogGroup::Desktop, false, None),
        "window.capture_screenshot" => ("窗口截图", ToolCatalogGroup::Desktop, true, None),
        "window.get_ui_tree" => ("窗口 UI 树", ToolCatalogGroup::Desktop, true, None),
        "window.observe" => ("观察窗口", ToolCatalogGroup::Desktop, true, None),
        "window.invoke" => ("调用界面元素", ToolCatalogGroup::Desktop, false, None),
        "window.set_value" => ("设置界面值", ToolCatalogGroup::Desktop, false, None),
        "window.toggle" => ("切换界面控件", ToolCatalogGroup::Desktop, false, None),
        "window.select" => ("选择界面元素", ToolCatalogGroup::Desktop, false, None),
        "window.wait" => ("等待窗口", ToolCatalogGroup::Desktop, true, None),
        "media.inspect" => ("检查媒体", ToolCatalogGroup::Media, true, None),
        "media.describe" => ("描述图像", ToolCatalogGroup::Media, true, None),
        "media.run_ocr" => ("媒体 OCR", ToolCatalogGroup::Media, true, None),
        "media.transcribe" => ("转录音频", ToolCatalogGroup::Media, true, None),
        "media.extract" => ("提取文档", ToolCatalogGroup::Media, true, None),
        "media.render" => ("渲染文档页", ToolCatalogGroup::Media, true, None),
        "media.generate" => ("生成图像", ToolCatalogGroup::Media, false, None),
        "media.record" => ("录音", ToolCatalogGroup::Media, false, None),
        "media.play" => ("播放音频", ToolCatalogGroup::Media, false, None),
        "media.speak" => ("语音朗读", ToolCatalogGroup::Media, false, None),
        "media.get_volume" => ("读取音量", ToolCatalogGroup::Media, true, None),
        "media.set_volume" => ("设置音量", ToolCatalogGroup::Media, false, None),
        "media.get_mute" => ("读取静音状态", ToolCatalogGroup::Media, true, None),
        "media.set_mute" => ("设置静音", ToolCatalogGroup::Media, false, None),
        "memory.search" => ("搜索记忆", ToolCatalogGroup::Haven, true, None),
        "memory.list" => ("列出记忆", ToolCatalogGroup::Haven, true, None),
        "memory.remember" => ("记住信息", ToolCatalogGroup::Haven, false, None),
        "memory.forget" => ("忘记信息", ToolCatalogGroup::Haven, false, None),
        "memory.recall" => ("召回记忆", ToolCatalogGroup::Haven, true, None),
        "agent.list" => ("列出 Agent", ToolCatalogGroup::Workflow, true, None),
        "agent.list_children" => ("列出子 Agent", ToolCatalogGroup::Workflow, true, None),
        "agent.read_history" => ("Agent 消息历史", ToolCatalogGroup::Workflow, true, None),
        "agent.list_inbox" => ("读取 Agent 消息", ToolCatalogGroup::Workflow, true, None),
        "agent.acknowledge" => ("确认 Agent 消息", ToolCatalogGroup::Workflow, false, None),
        "agent.send" => ("发送 Agent 消息", ToolCatalogGroup::Workflow, false, None),
        "agent.reply" => ("回复 Agent", ToolCatalogGroup::Workflow, false, None),
        "agent.get_profile" => ("Agent 资料", ToolCatalogGroup::Workflow, true, None),
        "agent.request" => ("请求 Agent", ToolCatalogGroup::Workflow, false, None),
        "agent.get_status" => ("Agent 状态", ToolCatalogGroup::Workflow, true, None),
        "agent.wait" => ("等待 Agent", ToolCatalogGroup::Workflow, true, None),
        "agent.stop" => ("停止 Agent", ToolCatalogGroup::Workflow, false, None),
        "agent.collect" => ("收集 Agent 结果", ToolCatalogGroup::Workflow, true, None),
        "session.create" => ("创建子会话", ToolCatalogGroup::Workflow, false, None),
        "session.link" => ("关联会话", ToolCatalogGroup::Workflow, false, None),
        "session.unlink" => ("取消会话关联", ToolCatalogGroup::Workflow, false, None),
        "session.list_links" => ("查看关联会话", ToolCatalogGroup::Workflow, true, None),
        "session.read_notes" => ("读取会话笔记", ToolCatalogGroup::Workflow, true, None),
        "session.replace_notes" => ("替换会话笔记", ToolCatalogGroup::Workflow, false, None),
        "session.search_history" => ("搜索原始会话历史", ToolCatalogGroup::Workflow, true, None),
        "session.read_history" => ("读取原始会话记录", ToolCatalogGroup::Workflow, true, None),
        "session.search_linked_history" => {
            ("搜索关联会话历史", ToolCatalogGroup::Workflow, true, None)
        }
        "session.read_linked_history" => {
            ("读取关联会话记录", ToolCatalogGroup::Workflow, true, None)
        }
        "session.search_assets" => ("搜索关联资产", ToolCatalogGroup::Workflow, true, None),
        "toolrun.start" => ("启动后台工具", ToolCatalogGroup::Workflow, false, None),
        "toolrun.list" => ("后台任务列表", ToolCatalogGroup::Workflow, true, None),
        "toolrun.inspect" => ("查看后台任务", ToolCatalogGroup::Workflow, true, None),
        "toolrun.cancel" => ("取消后台任务", ToolCatalogGroup::Workflow, false, None),
        "schedule.set" => ("设置定时任务", ToolCatalogGroup::Workflow, false, None),
        "schedule.list" => ("定时任务列表", ToolCatalogGroup::Workflow, true, None),
        "schedule.cancel" => ("取消定时任务", ToolCatalogGroup::Workflow, false, None),
        "system.get_info" => ("系统信息", ToolCatalogGroup::System, true, None),
        "system.list_displays" => ("显示器信息", ToolCatalogGroup::System, true, None),
        "system.env.list" => ("列出环境变量", ToolCatalogGroup::System, true, None),
        "system.env.get" => ("读取环境变量", ToolCatalogGroup::System, true, None),
        "system.registry.list" => ("列出注册表", ToolCatalogGroup::System, true, None),
        "system.registry.get" => ("读取注册表", ToolCatalogGroup::System, true, None),
        "system.power.get_status" => ("电源状态", ToolCatalogGroup::System, true, None),
        "system.power.lock" => ("锁定电脑", ToolCatalogGroup::System, false, None),
        "system.power.sleep" => ("睡眠", ToolCatalogGroup::System, false, None),
        "system.power.hibernate" => ("休眠", ToolCatalogGroup::System, false, None),
        "haven.diagnostics.get_status" => (
            "诊断状态",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.diagnostics.tail_logs" => (
            "诊断日志",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.diagnostics.list_sessions" => (
            "诊断会话",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.diagnostics.list_errors" => (
            "诊断错误",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.config.get" => (
            "读取配置",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Low),
        ),
        "haven.config.set_log_level" => (
            "设置日志级别",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.skills.list" => (
            "列出技能",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.skills.enable" => (
            "启用技能",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.skills.disable" => (
            "禁用技能",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.skills.create" => (
            "创建技能",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.tools.enable" => (
            "启用工具",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.tools.disable" => (
            "禁用工具",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.mcp.list" => (
            "列出 MCP",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.mcp.connect" => (
            "连接 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.mcp.disconnect" => (
            "断开 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.mcp.add" => (
            "添加 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.update" => (
            "更新 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.toggle" => (
            "切换 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.remove" => (
            "移除 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.reload" => (
            "重载 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        _ => (name, ToolCatalogGroup::Other, false, None),
    };

    let idempotency = match name {
        // Configuration toggles and lifecycle controls converge on a stable
        // state, so repeating the same request is safe.
        "toolrun.cancel"
        | "schedule.cancel"
        | "haven.config.get"
        | "haven.config.set_log_level"
        | "haven.skills.list"
        | "haven.skills.enable"
        | "haven.skills.disable"
        | "haven.tools.enable"
        | "haven.tools.disable"
        | "haven.mcp.list"
        | "haven.mcp.connect"
        | "haven.mcp.disconnect"
        | "haven.mcp.toggle" => OperationIdempotency::Idempotent,
        name if name.starts_with("lsp.") => OperationIdempotency::NonIdempotent,
        "session.link"
        | "session.unlink"
        | "session.read_notes"
        | "session.replace_notes"
        | "session.search_history"
        | "session.read_history"
        | "session.search_linked_history"
        | "session.read_linked_history"
        | "session.search_assets" => OperationIdempotency::Idempotent,
        // Creation and replacement/removal requests must be verified before
        // replay because their first attempt may already have changed state.
        "haven.skills.create"
        | "haven.mcp.add"
        | "haven.mcp.update"
        | "haven.mcp.remove"
        | "haven.mcp.reload" => OperationIdempotency::Unknown,
        _ if read_only => OperationIdempotency::Idempotent,
        _ => OperationIdempotency::NonIdempotent,
    };

    OperationContract {
        name,
        label,
        catalog_group,
        read_only,
        network_access_override: match name {
            // `status` probes configured provider endpoints with GET /models.
            "haven.diagnostics.get_status" => Some(NetworkAccess::Public),
            // These operations can establish or re-establish MCP connections.
            // Keep model-visible views and native typed requests on the same
            // conservative classification even when arguments may disable
            // auto-connect for a particular call.
            "haven.mcp.connect" | "haven.mcp.add" | "haven.mcp.update" | "haven.mcp.toggle"
            | "haven.mcp.reload" => Some(NetworkAccess::Opaque),
            // These operations only inspect local state or tear down an
            // existing client; they do not initiate a connection.
            "haven.mcp.list" | "haven.mcp.disconnect" | "haven.mcp.remove" => {
                Some(NetworkAccess::None)
            }
            "lsp.find_definition"
            | "lsp.find_implementations"
            | "lsp.find_type_definition"
            | "lsp.find_references"
            | "lsp.get_hover"
            | "lsp.list_document_symbols"
            | "lsp.search_workspace_symbols"
            | "lsp.get_diagnostics" => Some(NetworkAccess::Opaque),
            _ => None,
        },
        risk_override,
        idempotency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_contract_keeps_policy_and_presentation_together() {
        let contract = operation_contract("haven.mcp.add");
        assert_eq!(contract.label, "添加 MCP");
        assert_eq!(contract.catalog_group, ToolCatalogGroup::Haven);
        assert_eq!(contract.risk_override, Some(RiskLevel::High));
        assert!(!contract.read_only);

        for (name, group) in [
            ("files.read", ToolCatalogGroup::Files),
            ("clipboard.list_history", ToolCatalogGroup::Desktop),
            ("input.click", ToolCatalogGroup::Desktop),
            ("window.observe", ToolCatalogGroup::Desktop),
            ("system.get_info", ToolCatalogGroup::System),
            ("media.describe", ToolCatalogGroup::Media),
            ("memory.search", ToolCatalogGroup::Haven),
            ("agent.list", ToolCatalogGroup::Workflow),
            ("session.list_links", ToolCatalogGroup::Workflow),
            ("toolrun.start", ToolCatalogGroup::Workflow),
            ("schedule.set", ToolCatalogGroup::Workflow),
        ] {
            assert_eq!(operation_contract(name).catalog_group, group, "{name}");
        }

        let mcp_connect = operation_contract("haven.mcp.connect");
        assert_eq!(
            mcp_connect.network_access_override,
            Some(NetworkAccess::Opaque)
        );
        assert_eq!(
            operation_contract("haven.mcp.list").network_access_override,
            Some(NetworkAccess::None)
        );

        assert!(operation_contract("session.read_notes").read_only);
        assert!(operation_contract("session.search_history").read_only);
        assert!(operation_contract("session.read_history").read_only);
        assert!(operation_contract("session.search_linked_history").read_only);
        assert!(operation_contract("session.read_linked_history").read_only);
        assert!(operation_contract("session.search_assets").read_only);
        let replace_notes = operation_contract("session.replace_notes");
        assert!(!replace_notes.read_only);
        assert_eq!(replace_notes.idempotency, OperationIdempotency::Idempotent);

        for name in [
            "haven.mcp.add",
            "haven.mcp.update",
            "haven.mcp.toggle",
            "haven.mcp.reload",
        ] {
            assert_eq!(
                operation_contract(name).network_access_override,
                Some(NetworkAccess::Opaque),
                "{name} may connect an MCP server"
            );
        }
        for name in ["haven.mcp.list", "haven.mcp.disconnect", "haven.mcp.remove"] {
            assert_eq!(
                operation_contract(name).network_access_override,
                Some(NetworkAccess::None),
                "{name} does not initiate a connection"
            );
        }

        let status = operation_contract("haven.diagnostics.get_status");
        assert!(status.read_only);
        assert_eq!(status.network_access_override, Some(NetworkAccess::Public));

        let errors = operation_contract("haven.diagnostics.list_errors");
        assert!(errors.read_only);
        assert_eq!(errors.network_access_override, None);

        assert_eq!(
            operation_contract("session.link").idempotency,
            OperationIdempotency::Idempotent
        );
        assert_eq!(
            operation_contract("session.unlink").idempotency,
            OperationIdempotency::Idempotent
        );
        assert_eq!(
            operation_contract("session.create").idempotency,
            OperationIdempotency::NonIdempotent
        );
    }
}
