# ADR 1168：默认 Shell 工作目录复用 ExecutionWorkspace

## 状态

已采纳并实施。

## 背景

ADR 1167 将 Files 相对路径解析、`WorkspaceWrite` 写入根和 Agent prompt 统一到 Tools 授权 owner 的进程级 `ExecutionWorkspace`。Shell 默认 `cwd` 仍在每次调用时重新发现仓库根，会留下第二个可漂移的工作区解析来源。

## 决策

- Shell 未传入 `cwd` 时使用注册时从同一 AuthorizationEngine 取得的 `ExecutionWorkspace` 根。若没有可用执行根，则保留现有 Shell runtime 的共享 Temp 默认目录。
- 显式 `cwd` 保持原有解析语义，并继续经过标准授权网关的路径策略检查。
- `cwd` 仅控制进程启动目录，不会限制 Shell 命令后续读取或写入的绝对路径、环境可达路径或可执行子进程。它不是 OS filesystem sandbox。

## 影响与迁移

配置、IPC、持久化数据和显式 `cwd` 行为均不变；默认工作目录与文件工具和 Agent prompt 使用同一不可变快照。无需迁移或重置。

## 验证

- 定向测试覆盖注入 workspace 成为默认 `cwd`，以及显式 `cwd` 优先级。
- `cargo test --locked --offline -p haven-tools`：898 passed，2 ignored；MCP integration 7 passed。
- 严格 crate Clippy 被 5 个已有 warning 阻止（`registry.rs`、`mcp_server_config.rs`、`security.rs` 测试 helper、`skill_runner.rs`）；对这些 lint 显式放行后 `haven-tools` Clippy 通过。
