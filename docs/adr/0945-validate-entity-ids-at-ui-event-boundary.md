# 0945：在 UI event 边界校验实体 ID

## 状态

已接受并实现（2026-10-11）。

## 背景

Rust 使用 `haven_common::types::is_canonical_id` 校验 Haven 实体 ID。UI 的 session、Agent、ToolRun、recording 与 interaction event mapper 原先只检查非空字符串或字符串类型；错误前缀、短 ID、带连字符 UUID 和大写 hex 仍可能进入 store。录音错误/转写事件还会把缺失 ID 投影成空字符串。

## 决定

- 在 `contracts/wireGuards.ts` 集中提供 UI event 边界的 canonical `{prefix}-{uuid32}` 校验，并按字段语义校验 session、message、step、ToolRun、asset、recording、confirmation 与 call-preview ID。
- session lifecycle、Agent、ToolRun、recording 与 interaction mapper 对必需 ID 不规范的事件 fail closed；deleted session 仍允许 `null`，可选实体 ID 仅在省略或有效时接受。
- Provider `tool_call_id` / WebSearch `call_id` 继续保留外部格式，不套 Haven 前缀。
- 不改变有效事件的 wire shape、字段名或顺序；该校验只约束 renderer 入口。

## 影响

不规范事件不会进入前端 reducer/store/handler。Recording error 与 transcription event 不再用空 ID 构造貌似有效的 payload。该改动不涉及数据库、配置、Rust IPC DTO 或持久化迁移；无需重置数据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 替代方案

- 在每个 mapper 内各写一套格式检查：拒绝。会产生多套校验规则并造成前缀/UUID 长度漂移。
- 只依赖 Rust producer 的类型与构造：拒绝。renderer event mapper 接收 runtime wire value，既有 mapper 已承担运行时校验与 camelCase 投影；类型检查不能验证字符串内容。

## 回滚

若历史 renderer fixture 或外部集成依赖非规范事件 ID，应先修复 producer 或定义明确的 raw DTO 映射；不要把非规范 identity 放回业务 store。
