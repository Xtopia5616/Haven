# ADR 0530：类型化 ToolRun lifecycle events

## 状态

已采纳并实施（2026-10-06）。

## 背景

`ToolRunService` 通过 `Fn(String, Value)` 发布 lifecycle 事件，事件名和 payload 形状分散在 background/scheduled producer、App bootstrap 和 `event_bridge`。错误名称、缺字段和错误字段类型只能在 App 运行时发现。前端实际使用的是 App-owned `ToolRunEvent`，App mapper 会丢弃日志路径、工具参数等执行细节。

Tools 的 `ToolRunKind` 与 App 的 `ToolRunKind` 目前都包含 `Background` / `Scheduled`。前者表达 runtime 分类，后者是 Tauri DTO 的 wire vocabulary；相同取值不表示两者应共享所有权。

## 决定

1. Tools 的事件出口使用封闭的 `ToolRunLifecycleEvent` enum：`Created`、`Updated`、`Output`、`Finished`。Lifecycle payload 的 kind/status 均为 enum，ID、时间、来源和状态详情使用具名字段；output preview 有独立且更窄的 payload。producer 不再发任意事件字符串或 `serde_json::Value`。
2. Tools 拥有 runtime `ToolRunKind` 和 lifecycle event 生产；App 保留独立的 `ToolRunKind`、`ToolRunEvent` IPC DTO 与 projection。App 的枚举隔离 Tauri wire vocabulary，避免 Tools runtime 建模调整自动改变 renderer 契约。
3. App 对每个内部事件 variant 直接映射到既有 Tauri channel 和 DTO。Tauri wire payload 保持不变；ToolRun args、continuation prompt、dependency watch ID 与 log path 不属于 lifecycle event payload。
4. Foreground `agent:tool_output` 不是 ToolRun lifecycle event，保留独立的 sink state 和适配边界。

## 替代方案

- 两 crate 共用同一个 `ToolRunKind`：拒绝。App 对外 DTO 有独立 owner，复用 Tools runtime enum 会让内部建模决定 renderer 的 wire vocabulary。
- 保留 `Fn(String, Value)` 并只集中写字段文档：拒绝。producer 仍可发出未知事件名、错误状态和字段类型，契约不能由编译器约束。
- 直接序列化 Tools 的 lifecycle payload 到 Tauri：拒绝。那会让内部字段变更绕过 App 的 IPC field allowlist。

## 影响与验证

- Rust 内部的 sink 签名变更，App composition root 与现有 Tools/App 测试适配 typed event。Tauri event channels 和序列化 payload 不变，无前端改动、数据迁移或重置要求。
- 验收通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-adr-index.ps1`（513 条 ADR 索引且本地链接有效）与 `git diff --check`。

## 回滚

回滚本切片中 Tools event types、App typed mapper、适配代码、测试、架构清单和本 ADR，一起恢复内部 `(event, Value)` sink。Tauri wire contract 与持久数据均未变化，无数据回滚或重置步骤。
