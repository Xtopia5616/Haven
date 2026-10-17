# ADR 1293：在 Skill 激活结果中提供正文指令

## 状态

已采纳并实施。

## 背景

Haven 解析并保留 `SKILL.md` 的 Markdown 正文，但模型最初只收到 Skill 索引中的描述。`load_skill` 过去只注册 `skill__...` 工具并返回名称，因此正文指令没有进入模型上下文。system prompt 和能力索引按当前 run 冻结，不应为每次 session 工具激活重建。

## 决定

- `load_skill` 成功时，在工具结果的 `skill_instructions` 数组中返回所选 Skill 的完整正文。正文进入规范工具观察和 transcript，后续 provider 请求能够使用，恢复对话也保留该结果。
- 只为显式选择且当前仍启用、可执行的 Skill 返回正文；Skill adapter 仍按 session-scoped overlay 注册。
- 激活前按 `load_skill` 的有效输出上限检查完整 JSON 工具结果（全局 `max_observation_chars` 与该工具的 `max_output_chars` 中较小者）。若正文不能完整放入预算，则不注册任何所选 Skill，并返回缩短正文或减少选择的说明。
- Skill 正文是任务指引，遵从 Haven、system、user 指令及既有授权规则；该行为不扩大 Skill 的执行权限。

## 替代方案

- 把正文追加到全局 system prompt：拒绝，会破坏按需加载和 run 内 prompt 冻结语义。
- 把正文拼入工具 schema/description：拒绝，工具 schema 不是 Skill 正文的稳定承载边界，也会把可变正文混入 provider 工具定义。
- 保持正文仅本地解析：拒绝，模型无法按 Skill 作者提供的流程执行任务。

## 影响

成功的 `load_skill` transcript 观察会包含所选正文，受既有观察上限约束；超过上限的激活会失败关闭。没有数据库 schema、配置或 IPC wire 变化，也无需重置。Skills 仍是当前用户配置的能力，正文不会更改授权策略。

## 验证

- 现有 loader 用例更新为检查成功加载结果包含正文；本次未运行测试。
- `cargo fmt --all -- --check`、`cargo check --locked -p haven-tools` 与 `cargo clippy --locked -p haven-tools -- -D warnings` 通过。

## 回滚

回滚时移除 `skill_instructions` 输出和对应观察预算检查，并恢复此前只返回 Skill 名称的 loader 行为。既有 transcript 中已经保存的 Skill 正文仍作为普通工具结果保留，不需要数据重置。
