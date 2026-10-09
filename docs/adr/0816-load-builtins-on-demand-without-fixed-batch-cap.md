# ADR 0816：按需加载内置工具并取消固定批次上限

## 状态

Accepted — 2026-10-08

后续说明：工具请求预算的默认值由 [ADR 0820](0820-raise-default-tool-budget.md) 从 64 提高到 256；本文其余按需加载与无固定批次上限的决定继续有效。

## 背景

`tool_catalog(action=load)` 的 `operations` schema 将一次请求固定限制为 64 项，即使当前 deferred catalog 中有更多可用内置 operation。与此同时，`files.read`、`files.outline`、`files.search` 和 `system.info` 被放进常驻 provider surface，模型无需先从目录中选择就会收到它们的 schema。

## 决定

1. 移除内置 operation load 请求的固定 `maxItems` 限制。调用仍只能选择 host-owned deferred catalog 中已启用的内置 operation/root；未知 operation 仍按既有语义返回 `missing_operations`。
2. 保留 `context_limits.max_tools_per_request` 的单次模型请求预算和 `SessionToolOverlay` 的原子全有或全无准入。移除批次参数上限不绕过 provider 工具预算。
3. 常驻 provider surface 只包含 `ask`、`notify`、`tool_catalog`、`load_skill` 和 `load_mcp`。`files.read`、`files.outline`、`files.search` 与 `system.info` 留在 deferred catalog，由模型按需发现和加载；后续精选时再显式加入常驻列表。

## 替代方案

- 提高或删除 `max_tools_per_request`：拒绝。它保护实际发给 provider 的工具 schema 数量，与一次 loader 请求可携带多少选择不同。
- 保留四个 operation 常驻：拒绝。默认常驻集合应只提供控制、澄清和加载入口，避免未经筛选的操作直接进入每次模型请求。

## 影响与验证

这只改变模型可见工具 surface 和 loader 的动态输入 schema，不改变工具名称、权限、执行实现、配置或持久化数据；无需数据库或配置重置。验证覆盖超过 64 个选择项的 loader 请求被接受（未知名仍报告，匹配的 operation 正常加载），以及四个 operation 不再出现在重建后的全局 provider registry 中。

适用门禁：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`，并按 Git 流程检查差异。

回退时恢复四个常驻 operation 和 `operations.maxItems: 64` 即可，不需要迁移或重置数据。
