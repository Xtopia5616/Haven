# 0942：将 shell runtime helpers 限制在 Tools 内部

## 状态

已接受并实现（2026-10-11）。

## 背景

`build_shell_command` 和 `output_log_dir` 只在 `shell_runtime` 内部使用；`build_shell_command_silent`、`collect_byte_cap`、`proxy_env_vars`、`write_output_log` 和 Windows `CREATE_NO_WINDOW` 由 Tools 的 shell/ToolRun adapter 消费。调用图没有 App、Agent 或其他 crate 的消费者，但 Tools crate root 曾将这些实现 helper 作为公共 API 导出。

## 决定

- 保持 `shell_runtime` 模块私有，将跨 Tools 模块使用的 helper 与 Windows flag 限制为 `pub(crate)`，并将只在 `shell_runtime` 内调用的 `build_shell_command`/`output_log_dir` 设为私有。
- shell 命令构造、proxy 环境应用、输出容量、日志位置和 Windows 创建标志继续由 Tools 内部的 shell 与 ToolRun adapter 共同使用。
- 不新增替代 facade；下游无对应能力需求。

## 影响

这是未发布 workspace Rust API 的破坏性收口，仅删除无外部消费者的实现路径。进程 containment、shell 参数、取消、日志和输出行为均不变，无需数据重置。

## 验证

- 静态搜索确认这些 helpers 只在 `haven-tools` 内使用。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引及 `git diff --check`。

## 替代方案

- 保留 Tools root 的公共 helper 导出：拒绝。函数是 Tools 内部的 shell/runtime 实现，外部 crate 没有使用，公开它们只扩大可绕过领域 facade 的 API。

## 回滚

若其他 crate 未来需要 shell 能力，应通过受限领域 capability 或 ToolsFacade 操作表达；不要导出这些底层 process helper。
