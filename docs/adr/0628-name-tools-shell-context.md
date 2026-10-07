# ADR 0628：命名 Tools shell 执行上下文

## 状态

已采纳并实施。

## 背景

`ShellTool::resolve_shell_and_cwd` 同时解析 shell executable 与工作目录，返回 `(String, Option<PathBuf>)`。前台和后台执行路径都消费这个结果，tuple 位置让调用方需要记住第一个是 shell、第二个是 cwd。

## 决定

1. 将解析动作命名为 `resolve_shell_context`。
2. 以 `ResolvedShellContext { shell, working_directory }` 返回选定 shell 与可选工作目录。
3. 保留传入 shell 优先、否则使用 configured default，以及传入 cwd 优先、否则发现 workspace root 的规则。

## 替代方案

- 仅在调用点解构 tuple 并改局部变量名：拒绝，生产者与消费者仍无共享的上下文契约。
- 将默认 shell 与 cwd 拆成两个 resolver：拒绝，两者作为一条命令的执行上下文由同一调用点共同解析，且前后台路径共享该选择。

## 影响与验证

- 这是 Tools 私有 helper 的 Rust API 调整，Shell 工具输入、输出和执行行为不变。
- 命名审计 §5.7 继续覆盖 Rust 动词与多值结果；其它 crate、UI、IPC、配置和持久名仍待逐域审计。
- 验证：Tools fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `resolve_shell_and_cwd` 及其 tuple 返回值，并同步前后台执行调用点；无需数据或 wire 迁移。
