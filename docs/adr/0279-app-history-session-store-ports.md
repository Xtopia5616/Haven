# ADR 0279：App 历史查询通过 SessionStore blocking-pool ports

- 状态：Accepted
- 日期：2026-09-24
- 范围：Tauri 历史查询命令的 session history read path
- 关联：[ADR 0249](0249-session-store-session-record-reads.md)、[ADR 0277](0277-context-source-session-title-port.md)

## 背景

`commands/history.rs` 中的七个 async Tauri 命令直接调用同步 SQLite `Database`
查询。这些调用会在 Tokio async worker 上执行，并使 IPC adapter 依赖 raw Database。
查询实现已经集中在 `Database`，包含历史列表 cache、搜索谓词、排序和本地日期过滤。

## 决策

在 `SessionStore` 增加返回 typed `Session`/计数结果的历史查询端口，并通过
`Database::run_blocking` 调用原有的 `list_sessions`、`count_sessions`、搜索和过滤
方法。过滤条件使用 `SessionHistoryFilter` 表达；`limit` 和 `offset` 必须由调用者
提供，使历史页面默认值及 export 的 `10000/0` 边界继续由原命令明确负责。

应用组合根用同一个 `Arc<Database>` 创建一个 `SessionStore`，通过
`RuntimeServices` 注入 `ApplicationRuntime`，历史命令只依赖该 store。查询 SQL、谓词、
排序、cache、命令参数与返回 DTO、过滤默认值及 export JSON 形状均不改变；session
resume 命令的其他读取路径不在本 ADR 范围内。

`run_blocking` 会把 SQLite 工作提交到 Tokio blocking pool。调用方丢弃 async future
不能中断已经开始执行的 blocking closure；该端口不声明可取消语义。

## 影响与验证

- 历史命令不再直接持有或调用 raw `Database`，SQLite 查询不占用 Tokio async worker；
- SessionStore 端口测试覆盖各既有查询入口的结果与参数传递；
- 验收：`cargo fmt --all`、haven-memory 相关测试、haven-app-binary 测试，以及
  `cargo clippy --workspace --locked -- -D warnings`。

## 回滚

可以恢复历史命令的直接 Database 调用并移除新增的 SessionStore ports、注入字段和
filter 类型。此改动不写入新数据，不涉及 schema 或数据重置。
