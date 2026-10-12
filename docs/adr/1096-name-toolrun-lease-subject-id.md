# ADR 1096：以 subject ID 命名 ToolRun lease 身份

## 状态

已接受（2026-10-13）；取代 ADR 0676 中关于保留 `claim_token` 名称的决定。

## 背景

ADR 0676 将 `claim_token` 解释为 lease 内的 claim key，并据此保留旧名称。后续源码复核确认，`ToolRunLease::new` 的实参恒为正在租用的 ToolRun ID 或 completion result ID；字段不生成随机值，也不标识领取该实体的 consumer。`claim_token` 因而混淆了 lease 主体身份与 inbox 的一次性 claim token 概念。

## 决定

- 将字段及构造、claim、匹配和失效参数统一命名为 `lease_subject_id`；匹配方法使用 `matches_subject_id`。
- 调用点传入明确的 `tool_run_id` 或 `tool_run_result_id`；lease 的过期、CAS 和恢复行为保持不变。
- ADR 0676 保留为历史决策记录，并标注由本 ADR 取代。

## 影响

Common、Tools 内部 API 名称更准确表达身份关联，不再暗示其为不透明随机 token。序列化格式、SQLite 列、消息 wire shape 和运行时行为均不变。

## 验证

Common lease 单元测试与 Tools completion lease 测试验证 subject 匹配、过期重领和失效行为；运行 Common/Tools 相关测试与严格 Clippy。

## 回滚

若恢复旧名称，必须同步恢复“claim key 是 subject identity”的明确注释；不得把该值改成新的 consumer token 或持久格式。
