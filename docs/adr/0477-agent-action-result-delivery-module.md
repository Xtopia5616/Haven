# ADR 0477：Agent action-result delivery 私有模块

## 状态

已采纳并实施（2026-10-05）。

## 背景

`AgentLayer` 的启动文件同时装配 dispatcher、交互与 scheduled-fire consumer。background 和 scheduled-result 的 action-result consumer 虽从启动处注册，却承担一条独立的 Agent 侧投递流程：读取 terminal completion、构建不可信结果 envelope、按 session 生命周期选择入队或终态历史投影、重试 admission/projection、控制唤醒并发布后台任务通知。该路径在近期多次随 durable completion、恢复、终态投影与投递 retry 变更。

Action completion 的 durable outbox/transport 仍属于 Tools；live transcript durable projection 属于 ReAct/SessionStore。此次只整理 Agent consumer 的内部职责，不建立新的跨 crate owner。

## 决定

1. 将 background 与 `ScheduledResult` completion consumer、session status lookup、结果 envelope formatter 及 formatter 测试移至 `haven-agent` 私有模块 `layer/action_result_delivery.rs`。`AgentLayer` 只负责把 consumer 接入启动取消生命周期。
2. 新模块只拥有 Agent 侧投递编排。`ActionService` 继续拥有 completion transport、durable outbox 和 ack API；`SessionSupervisor` 继续拥有运行态、队列、状态与生命周期；ReAct/SessionStore 继续拥有 live transcript 投影及其 durable commit。
3. 保持既有 delivery 契约：live queue admission 不等于 outbox ack；稳定 `action_result_id` 重投不改变 transcript identity；live result 在 ReAct durable projection 后 ack；terminal session 在幂等 history-only projection 成功后 ack；取消、unowned、deleted session 的既有处理保持；等待 Ask/confirmation 的 Paused session 不因 action result 自动唤醒；仅 background completion 发布现有通知。
4. 不迁移 scheduled-fire execution、ActionStore/outbox 写入、SessionActor 状态或 ReAct projection；不添加 crate 依赖、公共 API、schema、wire、ID 或数据迁移。

## 替代方案

- 保持 consumer 与整个 startup body 混合：Agent 启动装配继续拥有大量具体交付策略，相关变更范围难以按职责审查。
- 新建独立 crate 或公共 service：当前没有独立消费者/API 边界，会扩大依赖图和组装成本。
- 把完整 Action/Job 生命周期、scheduled-fire 执行或 durable projection 一并搬移：会跨越 ADR 0325、0334、0393 定义的 owner，并有重复/丢失结果或过早 ack 风险。
- 只搬 envelope helper：不会形成完整交付边界，不能收敛主要 churn。

## 影响与验证

代码路径、ack 时序、事件与 transcript identity、session wake/notification 行为保持不变。没有 schema、配置、IPC、安全契约或用户数据迁移，无需数据库 reset。既有 Agent lifecycle、ReAct injection 与 Tools outbox 测试仍覆盖跨 owner 契约；纯 formatter 测试随 formatter 迁入新模块。

验证通过：

- `cargo fmt --all`（格式化）
- `cargo test --locked -p haven-agent`（569 passed，1 ignored）
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`
- `git diff --check`

## 回滚

可将 helper、formatter、consumer 与 formatter 单测移回 `layer.rs`，恢复原启动内联调用，并回退本 ADR/路线图记录。回滚不需要数据 reset；必须保持相同的 outbox ack 顺序、稳定 identity、终态幂等投影和 Ask 不自动唤醒规则。
