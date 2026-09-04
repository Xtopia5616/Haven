# ADR 0079：统一上下文预算与降级压缩边界

## 背景

上下文长度不只由 transcript 文本组成，还包括工具 schema、图片/音频、多轮
provider framing 和本次响应预算。原先的静态 `max_tokens`、字符数限制和压缩
失败即终止，会在工具很多、推理文本较长或 provider 窗口较小时出现误判；摘要
请求失败时也没有明确地告诉用户更早的内容已被省略。

## 决定

- 以 provider-visible token 预算作为统一边界：消息、工具 schema 和保守的
  provider overhead 一起参与 compaction preflight；每次 chat/stream 请求按剩余
  context window 动态计算正数的 output cap，并把 cap 传到 OpenAI、Responses、
  Anthropic 和 Gemini 适配器。
- 普通 compaction 使用结构化摘要提示，摘要输入/输出均有 token 上限；工具结果
  保留 `tool_call_id`，图片/音频以多模态 content part 送入具备能力的摘要 endpoint，
  不再只计数后静默丢弃。
- 摘要请求失败、为空或无法放入摘要 endpoint 时进入 degraded compaction：保留
  system、cache-friendly 的早期锚点和最近六条消息；最近尾部按完整 tool-call 边界
  选择，不保留孤立 tool result；删除更老的中间消息并插入稳定文本
  `[older context omitted]`。
- `degraded` 作为 transcript/event 的显式字段贯穿 Rust → Tauri → UI；旧快照和旧
  wire payload 缺少该字段时按 `false` 读取，degraded compaction 在 UI 显示 warning
  toast。
- token estimate cache 使用增量消息计数和流式 fingerprint 写入，健康的 canonical
  transcript 不再为预算计算复制整棵消息树；memory、session description、recent
  context 等上游 prompt 片段改用 token-aware 上限。
- compaction 摘要请求和 router one-shot 请求接收 `CancellationToken`；会话停止时
  立即丢弃未完成的维护请求，不继续占用 provider 请求槽位。

## 替代方案

- 继续使用固定 `max_tokens`：无法保证 input + output 不超过 provider window，拒绝。
- 仅按字符截断或仅删除最老消息：无法表达工具配对和多模态成本，容易生成 provider
  不接受的请求，拒绝。
- 摘要失败后直接结束会话：用户无法继续当前会话，改为确定性的有限信息降级路径。

## 影响

这是 Agent/LLM/IPC/UI 的跨 crate 契约变更，但不修改数据库 schema 或实体 ID 空间。
压缩后的 canonical transcript 仍由现有 transcript event 投影；新增的 `degraded`
字段有 serde/UI 兼容默认值。降级场景可能丢失较早中间细节，UI 会明确提示用户。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --locked -p haven-agent compactor::tests`
- `cargo test --locked -p haven-llm router::tests`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`

## 回滚 / 重置

回滚代码提交即可恢复旧的压缩/预算行为；本次不新增数据库迁移。旧 snapshot 的
`degraded` 缺省为 `false`，不需要用户重置数据。
