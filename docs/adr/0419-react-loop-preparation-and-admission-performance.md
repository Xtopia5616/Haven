# ADR 0419：ReAct 请求准备与工具准入性能

- 状态：Accepted
- 日期：2026-10-01
- 范围：`haven-agent` ReAct 请求准备、媒体摘要、工具 schema 与批次准入

## 背景

每轮请求都会从工具目录重新转换 provider schema 并序列化估算 schema token；健康的 canonical transcript 会被整段复制，媒体能力和媒体 part 数会重复扫描。工具批次的安全准入检查彼此独立，却按计划顺序逐个等待。

## 决定

1. ReActEngine 最多缓存 128 个 session 的 provider tool definitions 与 schema token estimate。键为工具目录的全局版本和 session overlay 版本；任一版本变化时必须 miss，命中时共享 definitions 的 `Arc`。缓存只持有运行时派生数据，不持久化。
2. `ReActState` 以 `Arc<Vec<CanonicalMessage>>` 持有 canonical projection。健康且无 retry-only 修改的请求共享该快照；需要修复或请求级改写时创建隔离副本。transcript append 更新 token/media 摘要，任意修改和 compaction 替换后重算摘要。
3. Text-only 请求跳过 media event replay 和空媒体映射分配。只有 canonical 中存在媒体 part 时才恢复媒体关联；adapter capability 需要替换媒体投影时仍创建请求级副本。
4. 工具准入检查以最多 8 个 future 并发运行，按 plan 顺序收集结果。所有准入决策完成前不启动任何工具；实际执行仍重新验证授权/receipt，工具结果仍按原计划顺序提交。准入 future 使用拥有输入，避免跨并发检查保留 transcript/plan 借用。

## 替代方案

- 每轮继续重建 schema、深拷贝 transcript 并重扫媒体：实现简单，但保留了重复准备成本。
- 并发启动工具执行而非只并发安全准入：会改变确认与批次屏障语义，因此不采用。
- 取消批次顺序约束或通过内容缓存授权结果：会改变可观察顺序或绕开 live authorization，因此不采用。

## 影响与验证

缓存占用有 128-session 上限，LRU 驱逐旧 entry；工具准入并发上限为 8。缓存版本 miss、容量驱逐、最大准入并发、计划顺序和“全部准入后才执行”均有边界测试。没有数据库、配置、IPC 或持久化行为变化；本 ADR 不声称特定生产延迟收益，实际会话中的收益需后续按真实链路测量。

验收命令：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

## 回滚

回退本 ADR 对应 ReAct cache、共享 canonical snapshot、增量媒体摘要与并发准入代码和测试即可。无数据重置要求。
