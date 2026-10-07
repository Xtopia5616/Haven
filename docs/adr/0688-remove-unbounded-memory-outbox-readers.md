# ADR 0688：删除无界 Memory outbox 元组读取入口

## 状态

已采纳并实施。

## 背景

Memory 的运行时 outbox 读取已由带 `high_water`、分页上限和取消语义的 `MemoryStore` API 提供，并返回 `FactExtractionMarker` / `SummaryExtractionMarker` 具名行。`Database::pending_fact_extractions` 与 `Database::pending_summary_extractions` 又把相同页面全部聚合成位置元组；全仓没有生产调用方，只供测试和一次性诊断使用。这两条公开包装重复了分页路径、丢失 marker 字段语义，并允许无界聚合。

## 决定

- 删除两个无界 `Database::pending_*_extractions` 包装，不保留兼容别名。
- 生产读取和测试读取都使用现有具名分页接口；测试 helper 只存在于各自 `#[cfg(test)]` 边界。
- `FactExtractionMarker` 与 `SummaryExtractionMarker` 是 outbox 查询行的唯一结构；fact/summary 的状态、身份与 ack 规则继续各由原有 owner 管理。

## 替代方案

- 继续保留全量扫描方法供测试使用：拒绝。测试可直接复用现有分页契约，生产不需要第二个未限量的 reader。
- 将元组改成新的全量具名列表 API：拒绝。仍会保留有界运行接口之外的整批物化路径。

## 影响与验证

- 删除 `haven-memory::Database` 的两个内部 API；Agent 与 Memory 测试改用有界分页读取。outbox marker 格式、事务、重试、ack、数据库 schema、Tauri/IPC 契约均不变。
- workspace 编译还发现两处测试引用未随既有类型移动更新：LLM circuit 类型仍用旧名，Agent Prompt 测试缺少 `PromptRuntimeContext` 导入；已按现有 owner 对齐，仅修复测试编译，不改变生产行为。
- 不涉及持久格式变更，无需数据库重置。
- 验证：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`。

## 回滚

无需数据回滚。若确需恢复诊断能力，应调用分页 API 并明确分页范围与上限，不恢复旧的无界元组包装。
