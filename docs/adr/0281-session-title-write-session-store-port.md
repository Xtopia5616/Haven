# ADR 0281：App 会话标题写入通过 SessionStore

- 状态：Accepted
- 日期：2026-09-24
- 范围：`update_session_title` Tauri command 的 session title 持久化路径
- 关联：[ADR 0279](0279-app-history-session-store-ports.md)

## 背景

`update_session_title` 是 async Tauri command，但直接同步调用 `Database::update_session_title`，
会在 Tokio async worker 上执行 SQLite 写入，并让命令越过 ApplicationRuntime 注入的存储边界。

## 决策

在既有 `SessionStore` 增加异步 typed port `update_session_title`，由它调用
`Database::run_blocking` 执行既有 `Database::update_session_title`。命令仍先 trim 和校验标题，
持久化成功后才调用 `executor.update_session_title`，随后按原样发出
`session_title_updated` 事件。命令名、错误映射、参数、返回值和 IPC payload 不变。

此端口沿用 `run_blocking` 的取消边界：调用方 future 被丢弃时，已开始执行的 blocking
closure 不会因此中断；命令取消后，SQLite 写入可能完成，而后续 executor 更新与事件发布不会继续。
端口保留既有 Database 错误及 session cache invalidation 行为。

此决定只覆盖标题写入；resume 的多表读取、`end_session` 和其他命令不在范围内。

## 影响与验证

- `update_session_title` 不再在 async command 中直接同步调用 SQLite；
- SessionStore 测试验证通过新端口更新标题，并确认历史列表缓存失效；
- 验收：`cargo fmt --all -- --check`、相关 Memory/Agent/App 测试、
  `cargo check --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings`。

## 回滚

恢复命令中的直接 Database 调用并删除新增的 SessionStore 端口与测试即可。该切片不改变 schema、
IPC 或持久化格式，无数据重置要求。
