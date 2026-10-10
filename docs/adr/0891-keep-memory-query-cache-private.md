# 0891：将 Memory 查询缓存实现限制在 Memory 内部

## 状态

已接受并实现（2026-10-10）。

## 背景

`Database` 的查询缓存、generation token 和 Memory revision 都由 Memory repository 读写路径管理，但这些辅助方法此前以公开方法挂在 crate 根导出的 `Database` 上。这样下游 crate 可以直接参与缓存读写/失效，绕过对应的 typed store，也让内部缓存机制成为跨 crate 契约。

调用图显示，生产代码中的缓存操作只发生在 Memory 内部。Tools 的三个外部调用位于单元测试：测试用 raw SQL 调整 session 时间戳后，需要清除 session-history fixture 缓存。

## 决定

- 将 Database 的缓存读取、写入、失效、generation 和 Memory revision 方法收为 crate 内部 API；`CacheGeneration` 不再从 crate 根导出。
- 仅在非默认 `test-support` feature 下保留 `cache_invalidate_session_history_page`，供 raw SQL 测试 fixture 清理缓存；生产构建只允许通过 SessionStore 写入以触发缓存失效。
- 保留 `Database` 作为 Memory 组合根的连接与 store 装配依赖；跨 crate 的数据库领域操作与 store 构造边界继续按实际消费者审查。

## 影响

- 外部生产 crate 无法直接读取、覆盖或失效 Memory 查询缓存，也不能把缓存 revision 当成跨层状态接口。
- 不改变缓存 TTL、失效顺序、SQL 事务或业务数据；无需数据库重置。
- 测试继续通过显式开启的 `test-support` 访问 raw SQLite，并可在 fixture 修改后使 session-history cache 失效。

## 验证

- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 保持公开方法并依赖调用约定：拒绝，因为生产 crate 仍可绕过 Memory store 所有权。
- 将缓存 helper 移到 App 或 Agent：拒绝，因为缓存读写和失效与 Memory SQL repository 的事务成功边界同属 Memory。
