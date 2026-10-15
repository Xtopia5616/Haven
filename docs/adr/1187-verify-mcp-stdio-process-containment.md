# ADR 1187：验证 MCP stdio 断开时的进程树回收

## 状态

已采纳并实施（2026-10-15）。

## 背景

ADR 0513 规定 MCP stdio 子进程在恢复执行前必须加入 kill-on-close Job Object；`StdioInner` 持有 child 和 containment 到 transport 关闭。Shell、Skill 与后台 Shell 已有 Windows adapter 级进程树回归测试，MCP stdio 仍需验证 drop 路径。

## 决定

1. 在独立 Windows 测试模块中通过 `McpClient::spawn_stdio` 启动 MCP stdio adapter。测试 PowerShell server 派生一个长驻 PowerShell 子进程并写入 PID；drop `StdioInner` 后必须观测到 server 与后代都退出。
2. 测试用父/子进程持有的临时独占文件句柄作为存活探针；失败清理写入 stop marker 让未被 Job 回收的测试进程有界退出，临时目录随后回收。
3. 测试不完成 MCP 协议握手，也不证明 stdio server 的文件系统或网络访问受到 OS 级隔离。它只验证 spawn/containment/drop 生命周期。
4. 生产代码、环境配置、MCP wire contract、持久数据和用户授权行为均未变化，无需重置。

## 替代方案

- 只测试 MCP server 直接 child 退出：不能覆盖其派生的后代，也不能证明 Job Object 在 `StdioInner` drop 时释放。
- 将进程树清理放入 MCP crate 的独立实现：拒绝。MCP 继续使用 `haven-platform::ProcessContainment` 作为唯一 Windows containment owner。
- 将 containment 结果解释为网络或文件系统权限隔离：拒绝。Job Object 仅控制受管进程树的生命周期。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked --offline -p haven-mcp client::stdio_process_tree_tests::dropping_stdio_transport_terminates_its_process_tree -- --exact --nocapture`
- `cargo check --workspace --locked --offline`
- `cargo clippy --workspace --locked --offline -- -D warnings`
- `cargo test --workspace --locked --offline --quiet -- --test-threads=4`
- ADR index、crate dependency inventory 与 `git diff --check`。

## 回滚

删除 MCP stdio adapter 回归测试、测试模块声明、本文和路线图中的 MCP 进程树验证记录。生产代码、配置和持久数据无需恢复。
