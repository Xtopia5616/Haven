# ADR 1184：验证 Shell 取消时的进程树回收

## 状态

已采纳并实施（2026-10-15）。

## 背景

ADR 0513 将 Windows Job Object 的创建、挂起启动、分配和恢复收敛到 `haven-platform::ProcessContainment`，并验证了平台原语能在关闭 Job 时终止子进程及后代。平台原语测试不能证明每个 adapter 都在正确的生命周期内持有 containment。路线图 §5.8 要求逐个验证 Shell、Skill 和 MCP stdio 的进程树边界。

## 决定

1. 为前台 Shell 增加 Windows adapter 级取消回归测试。Shell 启动一个受管子 PowerShell；子进程写出自己的 PID 并保持运行，测试取消 Shell 后必须观测到该后代进程退出。
2. 测试用临时目录保存 PID，等待和清理均有界；失败时按测试写出的 PID 尝试终止测试创建的进程。只启用 Windows 测试所需的 `windows-sys` Threading API feature，不增加 crate 依赖。
3. 该测试证明当前 Shell 调用路径持有 containment 到取消清理完成；它不证明 Skill、MCP stdio 或后台 Shell adapter 的同等性质，也不构成文件系统或网络隔离证明。
4. 生产执行行为、配置、IPC 和持久数据均未变化，无需重置。

## 替代方案

- 只保留平台 Job Object 单元测试：不足以发现 Shell adapter 提前释放 containment 或只终止直接子进程的问题。
- 把 Job Object 当作 OS 沙箱验收：拒绝。它限制受管进程树的生命周期，不约束进程文件系统或网络访问。
- 在 Shell 生产代码中增加另一层进程管理：拒绝。当前 containment owner 已在 spawn 前建立并跨越整个执行生命周期；本切片只补 adapter 级回归覆盖。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked --offline -p haven-tools cancelling_shell_terminates_its_descendant_processes -- --nocapture`
- 按安全/Windows 边界门禁运行适用的 workspace 检查与测试。

## 回滚

删除 Shell adapter 测试、Windows 测试所需 feature、本文及路线图中的 Shell 验证记录。无需恢复生产逻辑、配置或持久数据。
