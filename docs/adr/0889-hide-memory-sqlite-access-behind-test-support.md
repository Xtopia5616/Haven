# 0889：隐藏 Memory 的原始 SQLite 访问入口

## 状态

已接受并实现（2026-10-10）。

## 背景

生产组合根通过 `Database` 创建 `SessionStore`、Memory typed stores，再把这些能力注入 Agent、Tools 和应用运行时；生产调用路径没有跨 crate 的原始 SQLite 连接需求。此前 `haven-memory` 仍公开 `db`、`schema` 模块、`Database::conn` 与接收任意 `Database` 闭包的 `run_blocking`。Agent、Tools 和 App 的测试也复用这些生产入口执行 fixture SQL，导致底层实现边界与测试设施混在一起。

此外，带数据库引用的 `MemoryRetriever` 经 `recall` 模块和 crate 根公开。Agent 实际只调用它的可见性 helper，并不需要持有或调用具体检索器；检索执行已经由 Memory stores 拥有。

## 决定

- 将 `db` 与 `schema` 模块收为 Memory 内部模块。组合根仍可经根级 `Database` 创建生产数据库并装配 typed stores。
- 正常构建中，`Database::conn`、内存数据库构造器和任意 `run_blocking` closure 不属于下游生产 API。原始连接与闭包测试入口只在非默认 `test-support` feature 下可用；blocking 可取消入口只供 Memory 内部 stores 使用。
- 将 `MemoryRetriever` 收为 crate 内实现，不再从根级 API 暴露。事实可见性与安全来源片段过滤改由 `repositories::facts` 中的纯领域函数拥有，Agent 仅依赖这些函数和 typed result。
- Agent、Tools、App 的开发依赖按需启用 Memory `test-support`；各自正常依赖保持默认 feature 集。

## 影响

- 下游生产 crate 不能再通过公开 connection 或 blocking closure 绕过 Memory store boundary 执行任意 SQLite 操作。
- Memory 的测试仍可使用隔离内存数据库、SQL fixture 与 blocking 注入，不影响 schema、表结构、数据和运行时行为。
- `Database` 的领域级公开方法及所有 `repositories` 模块仍在后续 API 审计范围内；本 ADR 只收回底层连接、blocking callback 和具体 Retriever 泄漏，不表示 Memory API 审计已完成。
- 不需要数据库、配置或缓存重置：没有持久格式、schema 或数据语义变化。

## 验证

- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo fmt --all -- --check`

## 替代方案

- 继续让所有 crate 直接取得连接：拒绝，因为普通生产 API 可绕开 Memory 的事务与领域 store owner。
- 将测试 SQL fixture 全部改写成 store 操作：暂不采用；失败注入、SQLite schema 断言和池行为测试确实需要受控的原始连接，因此单独隔离在非默认测试 feature。
