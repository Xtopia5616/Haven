//! Canonical metadata for builtin operation views.
//!
//! The aggregate implementations own execution. This module owns the stable
//! operation identity metadata shared by the operation-view builder, policy
//! projection, prompt catalog and UI manifest. Schemas remain close to the
//! implementation because they can depend on runtime limits.

use crate::OperationIdempotency;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OperationContract {
    pub(crate) name: &'static str,
    pub(crate) label: &'static str,
    pub(crate) catalog_group: ToolCatalogGroup,
    pub(crate) read_only: bool,
    pub(crate) risk_override: Option<RiskLevel>,
    pub(crate) idempotency: OperationIdempotency,
}

/// Return the single stable metadata record for a model-facing builtin
/// operation. Unknown names stay readable and conservative, which keeps a
/// newly added operation visible while forcing an explicit contract entry for
/// read-only or elevated-risk behavior.
pub(crate) fn operation_contract(name: &'static str) -> OperationContract {
    let (label, catalog_group, read_only, risk_override) = match name {
        "files.read" => ("读取文件", ToolCatalogGroup::System, true, None),
        "files.outline" => ("文件大纲", ToolCatalogGroup::System, true, None),
        "files.summary" => ("文件摘要", ToolCatalogGroup::System, true, None),
        "files.search" => ("搜索文件", ToolCatalogGroup::System, true, None),
        "files.inspect" => ("检查文件元数据", ToolCatalogGroup::System, true, None),
        "files.stat" => ("读取文件状态", ToolCatalogGroup::System, true, None),
        "files.hash" => ("计算文件哈希", ToolCatalogGroup::System, true, None),
        "files.write" => ("写入文件", ToolCatalogGroup::System, false, None),
        "files.create_dir" => ("创建目录", ToolCatalogGroup::System, false, None),
        "files.edit" => ("编辑文件", ToolCatalogGroup::System, false, None),
        "files.patch" => ("批量精确编辑文件", ToolCatalogGroup::System, false, None),
        "files.copy" => ("复制文件", ToolCatalogGroup::System, false, None),
        "files.move" => ("移动文件", ToolCatalogGroup::System, false, None),
        "files.delete" => ("删除文件", ToolCatalogGroup::System, false, None),
        "files.list" => ("列出文件", ToolCatalogGroup::System, true, None),
        "process.list" => ("列出进程", ToolCatalogGroup::System, true, None),
        "process.kill" => ("终止进程", ToolCatalogGroup::System, false, None),
        "clipboard.read" => ("读取剪贴板", ToolCatalogGroup::System, true, None),
        "clipboard.write" => ("写入剪贴板", ToolCatalogGroup::System, false, None),
        "clipboard.history" => ("剪贴板历史", ToolCatalogGroup::System, true, None),
        "input.type" => ("输入文字", ToolCatalogGroup::System, false, None),
        "input.type_element" => ("向控件输入文字", ToolCatalogGroup::System, false, None),
        "input.key" => ("按键", ToolCatalogGroup::System, false, None),
        "input.click" => ("点击", ToolCatalogGroup::System, false, None),
        "input.click_element" => ("点击控件", ToolCatalogGroup::System, false, None),
        "input.move" => ("移动鼠标", ToolCatalogGroup::System, false, None),
        "input.scroll" => ("滚动", ToolCatalogGroup::System, false, None),
        "window.list" => ("列出窗口", ToolCatalogGroup::System, true, None),
        "window.foreground" => ("前台窗口", ToolCatalogGroup::System, true, None),
        "window.focus" => ("聚焦窗口", ToolCatalogGroup::System, false, None),
        "window.close" => ("关闭窗口", ToolCatalogGroup::System, false, None),
        "window.screenshot" => ("窗口截图", ToolCatalogGroup::System, true, None),
        "window.ocr" => ("窗口 OCR", ToolCatalogGroup::System, false, None),
        "window.ui_tree" => ("窗口 UI 树", ToolCatalogGroup::System, true, None),
        "window.observe" => ("观察窗口", ToolCatalogGroup::System, true, None),
        "window.invoke" => ("调用界面元素", ToolCatalogGroup::System, false, None),
        "window.set_value" => ("设置界面值", ToolCatalogGroup::System, false, None),
        "window.toggle" => ("切换界面控件", ToolCatalogGroup::System, false, None),
        "window.select" => ("选择界面元素", ToolCatalogGroup::System, false, None),
        "window.wait" => ("等待窗口", ToolCatalogGroup::System, true, None),
        "media.inspect" => ("检查媒体", ToolCatalogGroup::System, true, None),
        "media.describe" => ("描述图像", ToolCatalogGroup::System, true, None),
        "media.ocr" => ("媒体 OCR", ToolCatalogGroup::System, true, None),
        "media.transcribe" => ("转录音频", ToolCatalogGroup::System, true, None),
        "media.extract" => ("提取文档", ToolCatalogGroup::System, true, None),
        "media.render" => ("渲染文档页", ToolCatalogGroup::System, true, None),
        "media.generate" => ("生成图像", ToolCatalogGroup::System, false, None),
        "media.record" => ("录音", ToolCatalogGroup::System, false, None),
        "media.play" => ("播放音频", ToolCatalogGroup::System, false, None),
        "media.speak" => ("语音朗读", ToolCatalogGroup::System, false, None),
        "media.volume_get" => ("读取音量", ToolCatalogGroup::System, true, None),
        "media.volume_set" => ("设置音量", ToolCatalogGroup::System, false, None),
        "media.mute_get" => ("读取静音状态", ToolCatalogGroup::System, true, None),
        "media.mute_set" => ("设置静音", ToolCatalogGroup::System, false, None),
        "memory.search" => ("搜索记忆", ToolCatalogGroup::Haven, true, None),
        "memory.list" => ("列出记忆", ToolCatalogGroup::Haven, true, None),
        "memory.remember" => ("记住信息", ToolCatalogGroup::Haven, false, None),
        "memory.forget" => ("忘记信息", ToolCatalogGroup::Haven, false, None),
        "memory.recall" => ("召回记忆", ToolCatalogGroup::Haven, true, None),
        "agent.list" => ("列出 Agent", ToolCatalogGroup::Agent, true, None),
        "agent.children" => ("列出子 Agent", ToolCatalogGroup::Agent, true, None),
        "agent.history" => ("Agent 消息历史", ToolCatalogGroup::Agent, true, None),
        "agent.inbox" => ("读取 Agent 消息", ToolCatalogGroup::Agent, true, None),
        "agent.ack" => ("确认 Agent 消息", ToolCatalogGroup::Agent, false, None),
        "agent.send" => ("发送 Agent 消息", ToolCatalogGroup::Agent, false, None),
        "agent.reply" => ("回复 Agent", ToolCatalogGroup::Agent, false, None),
        "agent.profile" => ("Agent 资料", ToolCatalogGroup::Agent, true, None),
        "agent.request" => ("请求 Agent", ToolCatalogGroup::Agent, false, None),
        "agent.spawn" => ("创建 Agent", ToolCatalogGroup::Agent, false, None),
        "agent.status" => ("Agent 状态", ToolCatalogGroup::Agent, true, None),
        "agent.join" => ("等待 Agent 完成", ToolCatalogGroup::Agent, false, None),
        "agent.wait" => ("等待 Agent", ToolCatalogGroup::Agent, true, None),
        "agent.stop" => ("停止 Agent", ToolCatalogGroup::Agent, false, None),
        "agent.collect" => ("收集 Agent 结果", ToolCatalogGroup::Agent, true, None),
        "actions.list" => ("后台任务列表", ToolCatalogGroup::Haven, true, None),
        "actions.inspect" => ("查看后台任务", ToolCatalogGroup::Haven, true, None),
        "actions.cancel" => ("取消后台任务", ToolCatalogGroup::Haven, false, None),
        "schedule.set" => ("设置定时任务", ToolCatalogGroup::Haven, false, None),
        "schedule.list" => ("定时任务列表", ToolCatalogGroup::Haven, true, None),
        "schedule.cancel" => ("取消定时任务", ToolCatalogGroup::Haven, false, None),
        "preferences.get" => ("读取偏好", ToolCatalogGroup::Haven, true, None),
        "preferences.set" => ("设置偏好", ToolCatalogGroup::Haven, false, None),
        "preferences.clear" => ("清除偏好", ToolCatalogGroup::Haven, false, None),
        "preferences.list" => ("偏好列表", ToolCatalogGroup::Haven, true, None),
        "checklist.list" => ("检查清单", ToolCatalogGroup::Haven, true, None),
        "checklist.add" => ("添加清单项", ToolCatalogGroup::Haven, false, None),
        "checklist.update" => ("更新清单项", ToolCatalogGroup::Haven, false, None),
        "checklist.remove" => ("移除清单项", ToolCatalogGroup::Haven, false, None),
        "checklist.clear" => ("清空检查清单", ToolCatalogGroup::Haven, false, None),
        "system.info" => ("系统信息", ToolCatalogGroup::System, true, None),
        "system.display" => ("显示器信息", ToolCatalogGroup::System, true, None),
        "system.env.list" => ("列出环境变量", ToolCatalogGroup::System, true, None),
        "system.env.get" => ("读取环境变量", ToolCatalogGroup::System, true, None),
        "system.env.set" => ("设置环境变量", ToolCatalogGroup::System, false, None),
        "system.env.unset" => ("删除环境变量", ToolCatalogGroup::System, false, None),
        "system.registry.list" => ("列出注册表", ToolCatalogGroup::System, true, None),
        "system.registry.get" => ("读取注册表", ToolCatalogGroup::System, true, None),
        "system.registry.set" => ("设置注册表", ToolCatalogGroup::System, false, None),
        "system.registry.delete_value" => ("删除注册表值", ToolCatalogGroup::System, false, None),
        "system.registry.delete_key" => ("删除注册表键", ToolCatalogGroup::System, false, None),
        "system.power.status" => ("电源状态", ToolCatalogGroup::System, true, None),
        "system.power.lock" => ("锁定电脑", ToolCatalogGroup::System, false, None),
        "system.power.sleep" => ("睡眠", ToolCatalogGroup::System, false, None),
        "system.power.hibernate" => ("休眠", ToolCatalogGroup::System, false, None),
        "haven.diagnostics.status" => (
            "诊断状态",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.diagnostics.logs_tail" => (
            "诊断日志",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.diagnostics.sessions" => (
            "诊断会话",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.diagnostics.errors" => (
            "诊断错误",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.config.config_get" => (
            "读取配置",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Low),
        ),
        "haven.config.logs_level" => (
            "设置日志级别",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.skills.skills_list" => (
            "列出技能",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.skills.skill_enable" => (
            "启用技能",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.skills.skill_disable" => (
            "禁用技能",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.skills.skill_create" => (
            "创建技能",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.tools.tool_enable" => (
            "启用工具",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.tools.tool_disable" => (
            "禁用工具",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.mcp.mcp_list" => (
            "列出 MCP",
            ToolCatalogGroup::Haven,
            true,
            Some(RiskLevel::Low),
        ),
        "haven.mcp.mcp_connect" => (
            "连接 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.mcp.mcp_disconnect" => (
            "断开 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        "haven.mcp.mcp_add" => (
            "添加 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.mcp_update" => (
            "更新 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.mcp_toggle" => (
            "切换 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.mcp_remove" => (
            "移除 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::High),
        ),
        "haven.mcp.mcp_reload" => (
            "重载 MCP",
            ToolCatalogGroup::Haven,
            false,
            Some(RiskLevel::Medium),
        ),
        _ => (name, ToolCatalogGroup::Other, false, None),
    };

    let idempotency = match name {
        // Configuration toggles and lifecycle controls converge on a stable
        // state, so repeating the same request is safe.
        "actions.cancel"
        | "schedule.cancel"
        | "haven.config.config_get"
        | "haven.config.logs_level"
        | "haven.skills.skills_list"
        | "haven.skills.skill_enable"
        | "haven.skills.skill_disable"
        | "haven.tools.tool_enable"
        | "haven.tools.tool_disable"
        | "haven.mcp.mcp_list"
        | "haven.mcp.mcp_connect"
        | "haven.mcp.mcp_disconnect"
        | "haven.mcp.mcp_toggle"
        | "haven.mcp.mcp_reload" => OperationIdempotency::Idempotent,
        // Creation and replacement/removal requests must be verified before
        // replay because their first attempt may already have changed state.
        "haven.skills.skill_create"
        | "haven.mcp.mcp_add"
        | "haven.mcp.mcp_update"
        | "haven.mcp.mcp_remove" => OperationIdempotency::Unknown,
        _ if read_only => OperationIdempotency::Idempotent,
        _ => OperationIdempotency::NonIdempotent,
    };

    OperationContract {
        name,
        label,
        catalog_group,
        read_only,
        risk_override,
        idempotency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_contract_keeps_policy_and_presentation_together() {
        let contract = operation_contract("haven.mcp.mcp_add");
        assert_eq!(contract.label, "添加 MCP");
        assert_eq!(contract.catalog_group, ToolCatalogGroup::Haven);
        assert_eq!(contract.risk_override, Some(RiskLevel::High));
        assert!(!contract.read_only);
    }
}
