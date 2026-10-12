# ADR 1065：统一会话、模型与后台任务并发范围

## 状态

已接受（2026-10-12）

## 背景

并发设置已经在 UI 上有明确范围，但 Common 对 TOML 和 Settings 没有对应语义校验。手工编辑配置或直接调用 Settings IPC 可绕过控件，导致会话 admission、每模型 router semaphore 或后台 shell 子进程上限偏离 UI 所表达的范围。

## 决定

- `[session].max_concurrent` 的有效范围为 1–10，与 SettingsBehavior 控件一致；Agent runtime setter 复用 Common validator，拒绝值且不改变当前 admission limit。
- `[llm].max_concurrent_requests` 的有效范围为 1–16，与 ModelSettings 控件一致；router 构造时对直接 typed input 再施加 1–16 的边界。
- `[context_limits].max_running_background_tool_runs` 的有效范围为 0–64。64 与默认并发上限一致，限制本机 shell 子进程数量；0 明确关闭新的后台任务。
- Common 同时验证 TOML 与 Settings；UI 数字 parser 与控件复用相同范围。此三个字段由各自 owner 执行，不把不同运行队列合并成一个预算。

## 影响

超范围 TOML 按现有策略备份并使用默认配置；Settings 更新在 apply 前拒绝。背景任务上限 0 仍是有效的关闭设置；数据库与配置 key 结构不变。需要高于 64 个并行后台 shell 时须显式调整产品资源预算与相应 UI/测试契约。

## 验证

- Common 测试覆盖三项配置与 Settings 的边界值接受和越界拒绝。
- Agent 测试确认 setter 越界不修改运行时 limit；LLM 测试确认 router 直接输入限制在共享范围内。
- UI contract 测试覆盖三项上下界，Svelte 控件使用同一导出范围。
- Workspace Rust 检查、clippy、全量测试，UI check/全量测试和 ADR 索引检查通过。
