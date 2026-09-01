# ADR 0066：有界缓存与上下文一致性

## 背景

缓存分散在 Memory 的 `Database`、Agent 的 prompt/Token 逻辑和 Tools 的
会话工具定义路径中，原有实现存在几类共同问题：SQLite 查询缓存没有容量上限，
全局 epoch 会让无关 session 一起失效；prompt 记忆缓存只有一个槽位；工具定义
缓存只看全局目录版本；token 缓存只看消息数量，等长替换可能继续使用旧估算。
这些问题同时影响命中率、内存边界和上下文一致性。

## 决定

- 将查询缓存机制提取到 `haven_memory::cache::QueryCacheStore`。它只负责 TTL、
  有界 LRU 和 generation，不负责 SQL；`Database` 保留 repository-facing facade
  和写路径上的失效时机。查询结果最多保留 256 个逻辑 key，消息、session、事实、
  embedding 使用独立 slot。
- 查询开始前捕获 `CacheGeneration`，写回时同时校验 key generation 与 domain
  generation。定向失效只影响对应 key，bulk 失效只影响对应 domain；并发失效后
  到达的旧查询结果被丢弃，避免 stale overwrite。generation 元数据即使 key 当前
  没有缓存条目也保留，以覆盖首次 miss 的竞态。
- prompt memory 使用 32 项 LRU，key 包含规范化后的 query、embedding model、
  memory revision 和 excluded session。空白差异不会制造重复 key；embedding
  获取失败时仍可用关键词结果完成本轮，但不把降级结果写入缓存。
- 工具定义缓存使用 `(global_catalog_version, session_overlay_version)` 二元版本，
  并最多保留 128 个 session。一个 session 加载 skill/MCP 不再令其他 session 的
  定义缓存同时失效；全局目录重建仍使所有 session 的派生定义失效。渐进式加载工具
  与 `ToolsManager` 共享 session overlay 版本。
- canonical token estimate 使用完整内容指纹。合法 append 继续沿用 prefix 的
  增量计算，并最多保留 256 个 session；内容替换、回滚、修复或压缩导致指纹不匹配
  时重新完整估算。
- 所有进程内缓存只在短暂的同步锁区间内读写，不在锁内执行 SQL、await 或 provider
  调用；缓存本身没有异步任务。被取消的外层读取即使已经进入 blocking 查询，晚到
  的写回仍由 generation 校验保护。

## 替代方案

- 继续使用进程级全局 epoch：实现简单，但无关 session 的写入会造成大量假 miss，
  并放大数据库和工具目录压力，拒绝。
- 只增加 TTL、不增加容量和 LRU：可以限制时间上的陈旧，但无法控制访问大量
  session/subject 后的内存增长，拒绝。
- 继续用消息数量或固定周期校准 token：周期窗口内仍会接受等长内容替换的旧值，
  而完整重算又浪费合法 append 的增量路径，拒绝。
- 把 embedding 失败后的关键词降级结果缓存：短期命中率更高，但会把临时 provider
  故障固化为上下文结果，拒绝。

## 影响与验证

这是 Agent、Tools、Memory 的进程内缓存重构，不修改 SQLite schema、snapshot、
IPC 或 provider wire 数据，也不需要迁移用户数据。结果缓存拥有明确容量；失效
和 generation 元数据保留是为了覆盖无条目 key 的并发竞态。

重点测试包括：查询缓存 LRU 容量与压力边界、key/domain 失效隔离、失效后的 stale
write 丢弃、prompt memory LRU 边界、token 等长替换与 append、以及 session 工具
版本不误伤其他 session。

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚 / 重置

回退本 ADR 对应提交即可恢复旧缓存路径；缓存均为进程内派生数据，不需要数据库
迁移或用户数据重置。回滚后重新启动进程即可清空新缓存状态。
