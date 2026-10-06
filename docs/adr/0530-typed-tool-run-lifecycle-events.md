# ADR 0530：类型化 ToolRun lifecycle events

## 状态

已采纳并实施（2026-10-06）。

## 背景

`ToolRunService` 通过 `Fn(String, Value)` 发布 lifecycle 事件，事件名和 payload 形状分散在 background/scheduled producer、App bootstrap 和 `event_bridge`。错误名称、缺字段和错误字段类型只能在 App 运行时发现。前端实际使用的是 App-owned `ToolRunEvent`，App mapper 会丢弃日志路径、工具参数等执行细节。

Tools 的 `ToolRunKind` 与 App 的 `ToolRunKind` 目前都包含 `Background` / `Scheduled`。前者表达 runtime 分类，后者是 Tauri DTO 的 wire vocabulary；相同取值不表示两者应共享所有权。

## 决定

1. Tools 的事件出口使用封闭的 `ToolRunLifecycleEvent` enum：`Created`、`Updated`、`Output`、`Finished`。Created/Finished 与状态更新都必须带生命周期 payload；`Updated` 将状态变化和后台 session attachment 区分为两种更新类型。Lifecycle payload 的 kind 为 enum，状态和对应时间由 `ToolRunLifecycleState` 一起表示；output preview 有独立且更窄的 payload。producer 不再发任意事件字符串或 `serde_json::Value`。
2. `ToolRunLifecyclePayload` 必须包含一个 `ToolRunLifecycleState`。`Running` 必须包含 `started_at`；完成和失败状态必须同时包含开始与结束时间；取消允许 `started_at` 缺省，以表达定时任务在等待期被取消。状态值不再与独立的可选时间字段组合，避免构造不一致的 lifecycle payload。无状态 metadata update 使用专属的 session-attachment payload。
3. Tools 拥有 runtime `ToolRunKind` 和 lifecycle event 生产；App 保留独立的 `ToolRunKind`、`ToolRunEvent` IPC DTO 与 projection。App 的枚举隔离 Tauri wire vocabulary，避免 Tools runtime 建模调整自动改变 renderer 契约。
4. App 对每个内部事件 variant 直接映射到既有 Tauri channel 和 DTO。Tauri wire payload 保持不变；ToolRun args、continuation prompt、dependency watch ID 与 log path 不属于 lifecycle event payload。
5. Foreground `agent:tool_output` 不是 ToolRun lifecycle event，保留独立的 sink state 和适配边界。

## 替代方案

- 两 crate 共用同一个 `ToolRunKind`：拒绝。App 对外 DTO 有独立 owner，复用 Tools runtime enum 会让内部建模决定 renderer 的 wire vocabulary。
- 保留 `Fn(String, Value)` 并只集中写字段文档：拒绝。producer 仍可发出未知事件名、错误状态和字段类型，契约不能由编译器约束。
- 直接序列化 Tools 的 lifecycle payload 到 Tauri：拒绝。那会让内部字段变更绕过 App 的 IPC field allowlist。
- 保留彼此独立的 `status`、`started_at`、`finished_at` 可选字段：拒绝。它们可以形成状态和时间互相矛盾的内部事件，且无法保证 `running` 必带开始时间。

## 影响与验证

- Rust 内部的 sink 使用 lifecycle state enum 绑定状态与时间，App projection 负责展平为既有 DTO。scheduled `running` sink 测试验证开始时间；Tauri event channels 和序列化 payload 不变，无前端改动、数据迁移或重置要求。
- 初始 typed-sink 切片验收通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-adr-index.ps1`（513 条 ADR 索引且本地链接有效）与 `git diff --check`。本次 lifecycle state 扩展的验证在变更完成后记录。

## 回滚

回滚本切片中 Tools event types、App typed mapper、适配代码、测试、架构清单和本 ADR，一起恢复内部 `(event, Value)` sink。Tauri wire contract 与持久数据均未变化，无数据回滚或重置步骤。
