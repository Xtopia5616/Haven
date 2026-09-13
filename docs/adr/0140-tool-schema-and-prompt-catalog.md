# ADR 0140：工具 Schema 与 Prompt 目录收敛

## 状态

Accepted — 2026-09-13

## 背景

模型每一步同时收到完整的 provider `tools[]` 和系统 Prompt 中的工具索引。
此前索引按工具逐条复制名称与描述，聚合 operation 的使用提示虽然已经位于
`OperationViewContract`，却没有进入 Prompt；窄 operation schema 也缺少视图级
标题和说明。媒体结果在受限 observation 中还可能先看到正文，才看到继续操作所需
的 `asset_id`。

## 决定

1. `ToolDef` 增加 prompt-only 的 `ToolPrompt`，包含何时使用、何时不用和关键
   operation。它不进入 `ToolDef::json()` 或 provider 参数，避免扩大已有 wire 契约。
2. operation view 从同一 contract 生成 `ToolPrompt`，并给窄 schema 补充 `title`
   与 `description`；原始分支校验、固定 discriminator、授权和执行路径不变。
3. Agent 系统 Prompt 将内置工具按 family 聚合为三类目录字段：`when_to_use`、
   `when_not_to_use`、`key_operations`。完整名称、参数和实时可用性仍以每一步的
   `tools[]` 为唯一权威；目录内容会清洗控制字符并限制长度。
4. 结构化 observation 将 `asset_id` 提升到恢复字段的优先位置，使媒体、截图和
   附件结果在预算不足时仍能继续传递给后续操作。

## 影响与验证

本变更不修改数据库、配置或持久化消息；也不改变 provider-facing tool schema
之外的执行边界。新增回归测试覆盖 prompt 目录聚合与清洗、operation schema 元数据、
ToolPrompt 不进入 wire JSON，以及 `asset_id` 的 observation 顺序。

验证命令：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-common -p haven-tools -p haven-agent
cargo clippy --workspace --locked -- -D warnings
```

回退对应代码和本 ADR 即可，无需数据重置。
