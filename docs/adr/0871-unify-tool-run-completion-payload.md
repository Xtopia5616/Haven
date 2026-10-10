# ADR 0871：统一 ToolRun 终态交付 payload

## 状态

Accepted — 2026-10-10

## 背景

ToolRun 的终态结果有三种生产路径：Tools 为在线后台完成构造临时通知，Memory 在 scheduled ToolRun 完成事务中写 outbox，Memory 在进程重启后从 terminal ToolRun row 重建缺失 outbox。三处都产生固定字段集合，却分别使用动态 JSON 或手工拼接；Agent 再通过字符串键从 `Value` 读取 output、error、error reason、log path 和 source step。后台的 durable 写 API 还把 output、error、时间等字段与完整 `status_json` 同时作为参数传入，允许两份数据不一致。

ToolRun status/list 则是另一种投影：Tools 的 `ToolRunStatusView` 控制 UI 字段和运行态可见性，不能成为完成交付 DTO 的 owner。

## 决定

- 在 Common 定义 `ToolRunCompletionPayload`，集中声明 terminal completion 的 ID、status、output/error、时间、截断标记与可选来源字段。后台 projection 中已有的可选 `kind` 标签保留为序列化字段；Tools runtime 的 completion kind 仍由 Tools 及其事件 variant 拥有。
- Tools 的后台与 scheduled live completion、Memory 的 scheduled outbox 写入和 crash reconciliation 均构造同一 DTO；Tools completion events 与 Memory outbox row 都以具名 `payload` 暴露它，Agent 直接读取 typed fields。
- Memory 的 `ToolRunStore::finish_background_tool_run_with_completion` 只接收 DTO。数据库事务从该值读取 row 更新字段，并序列化同一个值写入现有 `status_json` 列，删除拆分参数与动态 JSON 之间的双重来源。
- `status_json` 只保留为 SQLite 列名和序列化边界名称；它不再是 Rust completion event/API 字段名。Tools `ToolRunStatusView` 与 status command 的 JSON projection 保持独立。
- 不把 Tools `ToolRunKind` 移入 Common，也不改变 App 的 IPC DTO owner。

## 替代方案

- 继续在各 producer 手工组装 `Value`：拒绝，会继续让固定 shape 靠重复键名与消费者约定维持。
- 把 `ToolRunStatusView` 直接传给 Agent：拒绝，UI status fields 与 durable result delivery 有不同的字段可见性、生命周期和消费者。
- 让 Common 持有 Tools 的 runtime kind enum：拒绝，完成事件的 kind 仍属于 Tools 运行时，App 也有独立 wire enum。

## 影响与验证

- Rust 内部 `status_json: Value` 字段改为 `payload: ToolRunCompletionPayload`；Agent 不再按字符串键读取 completion data。
- SQLite 仍使用 schema v39 的 `status_json` TEXT 列；已有 payload 的 JSON 字段名与 optional omission 规则保持一致，无 IPC、配置、安全或持久化 shape 变更，无数据库重置要求。
- `cargo fmt --all -- --check` 与 `cargo check --workspace --locked --tests` 通过；没有运行测试套件。全 workspace strict Clippy 被既有 warnings 阻断，包含 Common/LLM/Messaging/Skills/Memory 测试目标的问题；本 ADR 的 Memory test helper 触发的参数数量 lint 已改为较小签名。

## 回滚

如后续需要改变持久 payload JSON shape，应单独更新数据库 schema version 与 release/reset 说明。不要恢复基于字符串键读取的 Agent completion consumer 或从 UI status view 构造终态交付。
