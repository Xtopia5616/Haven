# ADR 1192：验证过滤式环境下的 Windows Shell 兼容性

## 状态

已采纳并实施。

## 背景

ADR 1188 让 Shell 子进程从运行基线、用户上下文/区域设置和显式代理变量构建环境。纯过滤器测试与测试程序子进程探针能证明变量保留规则，但不能证明过滤后的真实 Windows 环境仍能启动常见 Shell、解析系统命令并在独立工作目录内操作文件。

## 决定

- 增加 Windows-only 子进程回归测试，使用 `build_shell_command_silent` 的实际环境过滤和命令构造路径，不修改测试进程的全局环境。
- 分别启动 `cmd` 与 Windows PowerShell，验证两者可以运行；PowerShell 还验证 `SystemRoot`、`TEMP`、`TMP`、`USERPROFILE` 可用，且能从保留的 `PATH` 解析 `cmd.exe`、`where.exe` 和 `whoami.exe`。
- PowerShell 通过 `COMSPEC` 启动 `cmd.exe`，验证常见子命令链路；两个 Shell 都在测试临时工作目录中运行，PowerShell 在其中完成文件写入、读取和清理。
- 测试不联网、不修改父进程或用户配置、不触碰真实用户文件；这只覆盖所列系统命令与基本工作目录操作，不代表所有 CLI、用户自定义环境依赖或 AppContainer 兼容性。
- 该验证不改变安全语义：环境白名单是凭据传递卫生措施，不限制子进程的同用户文件和网络权限，也不构成 OS sandbox。

## 替代方案

- 只测纯过滤器和子进程变量探针：拒绝，无法发现 Shell 可执行文件解析或标准环境缺失导致的真实启动失败。
- 运行网络安装、用户脚本或 Cargo 构建来探测兼容性：拒绝，范围大且依赖外部网络、用户配置或缓存，不适合作为稳定回归测试。
- 把通过少数命令测试解释为任意 Shell 兼容性：拒绝，实际只验证 ADR 所列系统命令与临时目录操作。

## 影响与迁移

仅增加 Windows 测试和验证记录，不改变配置、IPC、数据库或运行时执行权限；无需数据重置。后续过滤器变更应继续在 Windows 下运行该回归测试。

## 验证

- Windows 下针对性测试 `shell_runtime::tests::filtered_shell_environment_supports_common_windows_commands`。
- `haven-tools` crate 测试、check 与严格 Clippy。
- workspace 格式、ADR 索引、crate dependency inventory 与 `git diff --check`。

## 回滚

删除 Windows 兼容性测试、本文和路线图中的 ADR 1192 引用。无需恢复生产逻辑或持久数据。
