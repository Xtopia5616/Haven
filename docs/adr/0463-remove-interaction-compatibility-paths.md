# ADR 0463：删除交互请求的无类型入口与旧字段兼容

## 状态

已采纳（2026-10-04）。

## 背景

`InteractionRequest::new(session_id, kind, correlation_ids)` 按 `kind` 生成 ID，却把所有请求都构造成 `InteractionDetails::Generic`。因此调用方可以创建 Ask、Confirm 或 ScheduledConfirm 生命周期，却没有相应的选项、工具调用或授权数据。仓库内该构造器只被测试夹具使用；生产路径已经使用具体构造器。

`InteractionRequest` 还保留了 `prompt` 反序列化别名：新事件从不写该字段，但 resume 为旧的持久 event 读取它。这是当前事件恢复契约中最后一条仅供旧格式使用的分支。

## 决定

1. 删除通用 `InteractionRequest::new` 与 `InteractionDetails::Generic`，调用方必须选择 `ask`、`confirm`、`ui_confirm` 或 `scheduled_confirm`，让生命周期种类与详情保持一致。历史记录显示 Generic 只由被删除的测试构造器创建，生产从未写入该变体。
2. 删除旧 `prompt` 字段的反序列化兼容。schema 从 v35 升至 v36；打开 v35 数据库会按既有策略拒绝，用户按发布重置说明清理数据库后重新启动。新事件 schema 不再包含旧字段接受分支。
3. 将现存测试夹具改为使用具体 Ask/Confirm 构造器，覆盖有效请求的生命周期与恢复路径。

## 替代方案

- 保留通用构造器并要求调用者后续补详情：拒绝。中间状态可进入 registry 或恢复流，且构造器没有生产调用者。
- 保留 `Generic` 作为兼容变体：拒绝。Git 历史确认生产从未创建或持久化该变体。
- 保留旧 `prompt` 解码并继续沿用 v35：拒绝。这样会让无调用者的旧 payload 继续扩大 durable contract；测试版允许以完整重置界定新 contract。

## 影响与验证

Agent 内部 API 不再提供能生成缺少领域详情的交互请求入口；interaction event 不再反序列化旧 `prompt` 字段。schema v35 数据库要求完整重置，配置无需删除。

验证：静态搜索和 Git 历史确认该构造器/变体只有测试入口；更新了既有生命周期及恢复测试夹具并删除 app 的空投影分支。`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent -p haven-app-binary`、对应 crate 的严格 Clippy 与 `git diff --check` 通过。本轮未运行测试、UI 检查或生产构建。改变旧 event 解码与 schema 版本的回归测试尚未运行。

## 回滚

代码回退后，schema 常量仍为 v36；旧二进制要求 v35，因此切回旧版本前仍需重建数据库。若需要保留数据，只能在本变更发布前恢复 v35 数据根备份并配合旧二进制使用。配置无须重置。
