# ADR 0059：工具批处理调度与观察结果投影

## 背景

ReAct 工具批次原先把所有调用直接放入 `FuturesUnordered`。批次规模由模型响应
决定，取消时只能统一补写失败结果；同时 agent 对 observation 截断一次，
`execute_step` 写入 `session_steps` 却保存未截断摘要，导致正常运行、恢复和
snapshot-less 恢复看到的结果长度不同。

## 决定

- 用 `buffer_unordered(MAX_CONCURRENT_TOOL_CALLS)` 建立有界执行窗口，结合
  `CancellationToken` 在批次内取消；结果继续按 assistant tool-call 索引缓存并按
  canonical 顺序物化。
- 工具通过 `ToolConcurrency` 声明 `ReadOnly`、`Resource(key)` 或 `Exclusive`。
  安全只读工具默认可并行；资源写入按 key 串行；未声明的非安全工具默认全局串行。
- `ToolsManager::observation_text` 是 ToolResult 摘要的唯一容量入口，canonical、
  transcript history、session_steps 与 resume 都使用同一个有界文本。
- action step 生命周期为 `pending → running → completed/failed`；尚未开始的取消
  为 `cancelled`，可能已经越过外部副作用边界的中断为 `unknown`，禁止将后者自动
  当作可安全重试的失败。
- snapshot-less 恢复不再按 observation 内容匹配旧消息；它只使用 step projection
  并生成 `resumed_{step_id}` 本地 call id。有效 snapshot 仍由 `events` 单一权威恢复。

## 影响与重置

`session_steps.status` 增加 `cancelled` 与 `unknown`，schema v12 会自动迁移现有库；
不需要删除数据库。批次超过运行时上限的调用会以失败 observation 物化，不执行。

## 验证与回滚

验证命令：

```text
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

回滚对应提交即可；若只回滚部分代码，必须同步回滚 v12 step 状态迁移、调度器
结果状态和 observation 投影入口。
