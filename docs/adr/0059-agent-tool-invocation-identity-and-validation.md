# ADR 0059：Agent 工具调用身份与参数验证

## 背景

同一 ReAct step 可以包含多个并行工具调用。仅凭工具名和参数无法区分重复调用，
确认恢复、工具结果投影和无快照 resume 可能把一个调用的结果关联到另一个调用。
同时，使用 schema default、首个 enum 或类型占位符补齐参数，会在未得到模型明确意图
时改变有副作用工具的语义。

## 决定

- 每个调用使用三段身份：`step_id` 是执行行和 live card 的稳定身份，`action_index`
  是 assistant tool-call 数组内的零基顺序，`tool_call_id` 是 provider 调用身份。
  provider 返回空 ID 或重复 ID 时由 `new_id("call")` 生成唯一进程内调用 ID。
- `ReActSnapshot.events` 继续是 transcript 唯一权威；`session_steps` 物化保存
  `action_index` 与 `tool_call_id`，并按 `step_number, action_index, created_at, id`
  稳定排序。确认 pending、confirm callback、action/observation 事件都携带相同调用身份。
- 确认恢复必须按完整调用身份匹配，不能通过工具名、参数或 observation 文本反查。
  无快照恢复直接使用步骤列中的 `tool_call_id`；旧步骤缺失时只使用由持久步骤 ID
  派生的确定性 fallback。
- 工具执行前只调用工具自身的输入 validator。缺失字段、非法 enum、类型错误或非法
  optional 字段不改写原始 JSON，而是生成带工具名、`action_index` 和验证明细的结构化
  validation failure observation，交回 ReAct 模型处理。

## 替代方案

- 按工具名加参数匹配：重复调用不可区分，拒绝。
- 用 schema default、首个 enum 或类型占位符修复参数：可能改变副作用语义，拒绝。
- 只把 provider `tool_call_id` 当作身份：provider 可能返回空或重复 ID，且无法表达
  本地执行行与并行顺序，拒绝。

## 影响

数据库 schema 升至 v12，迁移为 `session_steps` 增加 `action_index` 和 `tool_call_id`。
旧步骤保持可读但属于 legacy fallback；正在等待确认的旧快照可能缺少完整身份，发布后
应清除并重建。`confirm:requested` 保留用于 resolve 的确认请求 `step_id`，另加
`invocation_step_id`、`action_index`、`tool_call_id`，以区分两种 ID 的用途。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
cd ui
corepack pnpm run check
corepack pnpm run test:run
```

回归覆盖重复 provider ID、并行调用顺序、无效参数不改写、确认快照身份 round-trip、
确认恢复的完整身份匹配，以及无快照重复工具调用的步骤身份投影。

## 回滚

回退本 ADR 对应提交即可恢复旧执行逻辑；若新版本已经迁移或写入 v12 数据，回退前需
恢复升级前的数据根目录备份。没有兼容性保证时，按发布说明删除整个 Haven 数据根目录，
不要混用新旧数据库、快照或 pending confirmation。
