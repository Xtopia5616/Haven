# ADR 0257：显式注入 ReAct 工具目录 port

- 状态：已采纳（2026-09-24）
- 范围：`AgentLayer` 装配 `ReActEngine` 时的 `ToolCatalogPort` 所有权
- 关联：[ADR 0224](0224-tool-catalog-port.md)

## 背景

ADR 0224 为 ReAct 引入了 `ToolCatalogPort`，但 `ReActEngine::new` 仍通过
`executor.get_tools()` 创建 `ToolsManagerToolCatalogAdapter`。因此 engine 构造仍
通过执行 facade 查找目录服务，port 没有由 composition root 明确提供。

## 决定

1. `AgentLayer::new` 是生产装配点：从 `SessionSupervisor` 取得一次
   `ToolsManager`，将同一实例交给 `SystemPromptBuilder` 和
   `ToolsManagerToolCatalogAdapter`。
2. `AgentLayer` 显式把 adapter 作为 `ToolCatalogPort` 传入
   `ReActEngine::new`。ReActEngine 只保存注入的 port，不调用
   `executor.get_tools()`，也不创建生产 adapter。
3. `ToolCatalogPort` 仍按原样接收 `session_id` 并返回 immutable
   `Arc<ToolCatalogSnapshot>`。目录内容、snapshot 版本语义、读取时点、执行和
   live authorization 均不变。
4. ReActEngine 对 executor、database、event、transcript 和工具执行的既有职责
   保持不变；`PromptContextProvider` / `SystemPromptBuilder` 对
   `ToolsManager` 的依赖不在本决定内。
5. engine 测试使用仅在 `cfg(test)` 下编译的便捷 adapter helper；port 行为测试
   在构造时注入 recording fake，并验证注入对象身份、`session_id` 原样传递和
   snapshot `Arc` 身份。

## 替代方案

- 继续由 ReActEngine 从 executor 获取 ToolsManager：构造保持简短，但保留隐式
  service locator，测试也需构造真实执行 facade 才能替换目录 port。
- 同时拆除 PromptContextProvider/SystemPromptBuilder 的 ToolsManager 依赖：会
  扩大到不同的 prompt 能力边界，本切片不需要该变化。

## 影响

- `ReActEngine::new` 现在要求调用方提供 `ToolCatalogPort`，构造入口收窄为
  crate 内装配与测试使用。
- 生产 composition root 只取得一次 ToolsManager，并显式共享给 prompt builder
  与目录 adapter。
- 无数据库、持久化、配置、IPC 或安全授权契约变化。

## 验证

- `cargo fmt --all`
- `cargo test --locked -p haven-agent`
- `cargo clippy --workspace --locked -- -D warnings`
- staged diff 通过 `git diff --cached --check`

## 回滚与重置

无持久化或配置变化，不需要数据重置。回滚时恢复 ReActEngine 内创建 adapter
的旧构造，并还原所有仓库内构造调用；ToolCatalogPort 的 session 与 snapshot
行为无需变化。
