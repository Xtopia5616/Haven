# ADR 0656：统一配置快照代次命名

## 背景

`ConfigService` 维护进程内单调递增的配置快照代次，并已将其命名为 `ConfigVersion`。App apply plan、prepared runtime、settings apply observation 及 Tools 的 MCP reconnect/refresh authorization 仍分别用裸 `u64` 表达相同值。配置日志级别工具结果也把配置代次输出为泛名 `version`，而 ToolRegistry 与其它 catalog 同样各有独立 version。

## 决定

- App runtime apply 结构、失败诊断和测试输入统一使用 `ConfigVersion`；`RuntimeConfigApplyPlan.version` 明确改为 `config_version`。
- Tools 的 `McpRefreshPlan`、native MCP reconnect authorization 和 log-level result 使用 Common 定义的 `ConfigVersion`。
- log-level 工具结果字段由 `version` 改为 `config_version`，不保留旧输出 alias。
- `ConfigSnapshot.version` / `ConfigChanged.version` 保留其 owner 上下文内的简洁字段名；ToolRegistry、session overlay、MCP/Skills catalog 各自的代次不映射到 `ConfigVersion`。
- 不改变代次生成、递增、stale authorization 判断或 apply 时序。持久配置、IPC command wire 和数据库均不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --locked -p haven-tools`
- `cargo test --locked -p haven-app-binary`
- `cargo clippy --workspace --locked -- -D warnings`

## 回滚与重置

代码回滚时恢复裸整数字段和 `version` 工具结果输出。本次无数据迁移；log-level ToolResult 是运行时工具结果，不保留兼容字段。
