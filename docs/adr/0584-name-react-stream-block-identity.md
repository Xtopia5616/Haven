# ADR 0584：为 ReAct 流式消息使用结构化身份

## 状态

已采纳并实施。

## 背景

`IdentityMap` 为一个 ReAct run 内的 thought/reasoning 流式块分配并复用 message ID。原 `StreamBlockKey = (u32, u64, &'static str)` 依次存 step、run 和裸字符串 kind；多个调用点重复传 `"thought"` / `"reasoning"`。字符串拼错时仍会落入默认 `msg-` 前缀，且 tuple 访问与参数名没有直接表达 step number 和 run identity。

## 决定

1. 用 `StreamBlockIdentity` 代替 tuple alias，身份由 step number、run ID 和闭合块类别组成。
2. 块类别限定为私有 `StreamBlockKind::{Thought, Reasoning}`，通过 `StreamBlockIdentity::thought` / `reasoning` 构造，禁止未识别字符串值。
3. IdentityMap 的 ensure、peek 和 fallback message-ID 方法使用完整的 `stream_block_*_message_id` 名称，并接收结构化 identity。
4. 保持 identity map 的 ReActState/run 生命周期；thought 使用 `step-` ID，reasoning 使用 `msg-` ID；重试复用、错误 partial 和持久化行为不变。

## 替代方案

- 只将 tuple alias 改成另一个 tuple alias：拒绝，位置语义和裸字符串类别仍会扩散到所有调用点。
- 将 kind 保留为 `&'static str`：拒绝，固定的两个类别应有闭合类型，避免拼写错误走默认分支。
- 将该索引提升到 session 或全局作用域：拒绝，身份目前只在单个 ReAct run 的生命周期内有效。

## 影响与验证

- 更新 Agent ReAct identity map、状态包装方法、stream、turn、turn-end 与 event-boundary 调用点及现有测试。
- 更新 `docs/naming.md` 的复合领域 key 规则与本路线图审计记录。
- 不改变数据库、session event、UI/Tauri payload 或消息 ID 前缀；无持久化、配置、IPC 或安全契约变化，无需重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-agent`、`cargo clippy --locked -p haven-agent -- -D warnings`、`cargo test --locked -p haven-agent -- --test-threads=1`（607 个单测通过；1 个手动性能测试及 2 个性能集成场景忽略）、ADR 索引及 `git diff --check`。

## 回滚

将 `StreamBlockIdentity` 与闭合类别恢复为旧 tuple alias / 字符串参数，并同步恢复 Agent ReAct 调用点；外部契约无需变更。
