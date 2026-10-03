# ADR 0439：共享滚动日志定位与有界尾部读取

- 状态：已采纳
- 关联：[ADR 0007](0007-settings-diagnostics-contracts.md)、[ADR 0370](0370-diagnostics-logging-command-contract-boundary.md)、[ADR 0391](0391-admin-services-typed-output-projections.md)
- 范围：设置页 `read_log_tail` 与模型工具 `haven.diagnostics.logs_tail`

## 背景

应用使用 daily rolling appender 写入 `{stem}.{YYYY-MM-DD}` 文件。设置页按该格式定位当前文件，而 Admin 工具曾直接读取配置中的基础路径，导致正常运行时找不到日志。Admin 工具还将整个文件载入内存；`limit` 只约束输出行数。日志关闭时，空路径又回退到默认路径，可能读出旧日志。

## 决定

1. `haven_common::log_file` 是滚动文件定位和尾部读取的唯一实现；设置页和 Admin 工具共用。
2. 工具只读取当前匹配的滚动文件。读取必须同时满足日志初始化的有效启用状态与 ConfigService 当前的 `file_enabled`；没有配置服务的构造场景只使用有效状态。关闭时返回现有 unavailable DTO 分支，不读取默认或历史日志。
3. 尾部原始数据按反向固定块读取，最多 2 MiB；超长、被截断的行不会以不完整内容泄露到工具输出。`total_lines` 仍需用固定大小缓冲区扫描整个文件，因此时间复杂度仍为 O(file size)，内存不随日志总大小增长。
4. `diagnostics_status.log.path` 与成功的 logs-tail 输出优先指向匹配到的实际滚动文件；未启用或尚无文件时仍返回配置路径。
5. 日志输出 DTO、参数名、行数上限、敏感 marker 过滤和文件读取错误分支保持不变；无当前滚动文件时返回 unavailable，错误为 `no log file found yet`。

## 影响与验证

公共 helper 位于 Common，供 App 命令和 Tools Admin service 复用。App 传入配置路径及日志初始化后的有效启用状态；Admin service 再结合 ConfigService 的当前设置做 fail-closed 判定。工具的文件系统工作放在 blocking worker，避免阻塞 Tokio executor。

覆盖路径包括 Common 的尾部/滚动文件 helper、Tools 的 Admin logs-tail contract，以及 App 的设置页日志命令。适用验证命令：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --locked -p haven-common
cargo test --locked -p haven-tools
cargo test --locked -p haven-app-binary
```

## 回滚

回退 Common log-file helper 与其调用点，恢复 App 内的本地 helper 和 Admin 的直接读取；同时移除 `AdminContext.file_logging_enabled`。不涉及 IPC/schema、配置迁移或用户数据重置。
