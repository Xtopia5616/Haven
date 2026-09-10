# ADR 0124：媒体推理用量与 Agent 缓存率边界

日期：2026-09-10
状态：已采纳

关联：[ADR 0102：聊天 token 统计明细与上下文快照](0102-token-usage-detail-popover.md)、
[ADR 0122：工具媒体请求统一入口](0122-unified-tool-media-entrypoints.md)、
[ADR 0123：Agent 原生媒体工具契约](0123-agent-native-media-tool-contract.md)

## 背景

媒体工具的图片描述、音频转写和窗口 OCR 可能在一次 Agent 工具调用中额外发起
LLM 请求。此前工具结果既可能重复携带派生文本，也可能把运行时元数据暴露给模型；
如果把这些内部请求与 Agent 主循环放进同一个缓存率分母，会把两个不同的 prompt
前缀和缓存策略混在一起，导致缓存率和累计 token 无法解释。

## 决定

1. 模型观察中的派生文本只有一个权威位置：`media.content`。不再同时写顶层文本和
   representation payload；运行时 hash、expiry、源路径、尺寸和 provider provenance
   不进入媒体工具的模型观察。
2. `ToolResult` 可以携带进程内的 `ToolLlmUsage`；媒体工具在成功的图片描述、音频转写
   和 OCR 后填充 endpoint role、provider usage、model 和 duration。该字段跳过工具结果
   JSON 序列化，只在 Agent 提交有序工具结果时处理。
3. `llm_usage.call_kind` 是持久化用量的封闭字段：`agent` 表示 Agent 主循环，`media`
   表示工具拥有的媒体推理。`session_usage` 只从 `agent` 行聚合；两类明细都可恢复和
   在 UI 中查看。
4. `agent:usage` 携带同一 `call_kind`。媒体事件只追加媒体明细，不更新工具栏的 Agent
   当前/累计统计；主循环缓存率仅使用 `call_kind=agent` 且 accounting 已知的行。

## 替代方案

- 把媒体调用并入 Agent 累计：实现简单，但会错误地把不同请求边界的 token 与缓存率
  混为一谈。
- 只在 UI 过滤媒体调用：数据库累计值和恢复态仍会被污染，刷新后会出现前后不一致。
- 继续重复返回派生文本：调用方容易实现，但扩大 prompt、增加截断概率并降低后续
  前缀复用，违反单一权威来源原则。

## 影响与安全边界

- schema 从 v17 提升到 v18；项目没有运行时迁移，升级需删除 `haven.db`、WAL 和 SHM，
  详见发布与数据重置说明。
- 媒体请求仍受原有资产 revalidation、字节上限、超时和不可信内容标记约束；usage
  只保存 token、缓存诊断、角色、模型、费用和耗时，不保存 prompt、原始 bytes 或 cache key。
- `media` 的 provider usage 可能是 unknown accounting；UI 不猜测媒体缓存率，且媒体
  调用不降低 Agent 主循环缓存率。当前无法从 native STT provider 获得 usage 时不伪造用量。

## 验证与回滚

- 工具单元测试验证 compact media reference 只含一个 content 槽位且不含运行时元数据。
- Memory 单元测试验证 media 明细保留但不进入 Agent session totals。
- UI 单元测试验证 media 行不进入累计 cache rate，并单独显示媒体调用数、token 和费用。
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cd ui; corepack pnpm run check; corepack pnpm run test:run`

回滚代码提交即可；若新版本已经写入 v18 数据，回退前必须按发布说明备份并删除数据库，
不得让旧二进制读取新的 `call_kind` schema。
