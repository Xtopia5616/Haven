# ADR 0743：Agent Observation 策略元数据复用 Common enum

## 状态

已采纳并实施。

## 背景

Agent observation 的 operation `idempotency` 与 `operation_scope` 在工具执行和 `ObservationCard` 中已经是 Common-owned enum，但写入 durable transcript UI annotation 时被转成字符串，恢复后又作为字符串进入 Agent event。App IPC DTO 和前端 `AgentObservationPayload` 也各自使用开放字符串/手写 union；事件 mapper 虽严格校验值，却维护了重复的值列表。

Durable `session_events.payload` 包含 `StoredObservationUi`，因此它同时是恢复契约。现存写入值来自固定的 `OperationIdempotency` 与 `ToolOperationScope` 变体。

## 决定

- `AgentEvent::Observation`、`StoredObservationUi` 和 App `AgentObservationEvent` 的 `idempotency` / `operation_scope` 字段直接使用 Common `OperationIdempotency` / `ToolOperationScope`。
- transcript 持久层直接序列化和反序列化这些 enum。snake_case JSON 字面量及现有 payload shape 保持不变。
- UI observation DTO 使用生成类型；runtime mapper 使用生成的 Rust 值清单并继续拒绝未知值（ADR 0380）。事件门禁检查这些类型和值清单不再退化为字符串副本。
- Tool result envelope 中历史 `retry_safety` 字段保持独立：其当前 observation 值沿用 idempotency 字面量，而 Common `ToolRetrySafety` 表达 `safe_to_retry` / `unsafe_to_retry`。本决定不混合这两个概念，也不改变 tool result payload。

## 替代方案

- 只类型化 IPC DTO、将 durable annotation 留作字符串：拒绝。durable payload 是 Observation event 恢复的来源，保留字符串会继续形成一个额外的无 owner 边界。
- 保留前端 union 和手写值列表：拒绝。Rust Common enum 已是 producer 和持久 annotation 的 owner，重复列表会漂移。
- 顺便重命名或改造 `retry_safety` envelope：拒绝。它是另一项独立语义和历史 wire 字段，不属于 observation policy metadata。

## 影响与验证

JSON 编码仍是 `idempotent` / `non_idempotent` / `unknown` 与 `global` / `session`，现有 durable payload 无需迁移或重置。未知枚举在持久 payload 解码和前端 event mapper 两处都严格失败。新增回归验证 durable value、publisher replay、未知 UI event 值拒绝和 IPC ownership gate。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 993 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（726 records）及 `git diff --check` 通过。IPC contract 检查包含生成 TypeScript `--check`。

## 回滚

如回滚，将 Agent/App event 字段和 `StoredObservationUi` 恢复为字符串，并在 transcript 构造时调用 enum 的 `as_str()`；恢复前端本地 union/值列表与门禁原状。由于 JSON payload 不变，无需数据库或用户数据重置。
