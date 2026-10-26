# ADR 1424：增加 SessionEnd 生命周期 Hook

本 ADR supersedes ADR 1371 中暂缓 `SessionEnd` 的决定。

## 状态

已实现；本次变更的自动化门禁已通过。

## 背景

现有 `run_end` 描述一次 Agent run 的完成、暂停、取消或错误。用户显式结束 Haven 会话时，SessionSupervisor 会将可继续的会话持久化为 `completed`，但没有对应的 Hook。ADR 1371 当时因缺少明确的 session terminal transition 而暂缓 `SessionEnd`；当前结束路径已由 SessionSupervisor 持有并提交该终态，可以在这个边界增加独立事件。

## 决定

- 增加通知型 `session_end`，纳入用户级 `config.toml`、受信项目 `.haven/hooks.toml`、设置编辑器、诊断和 IPC 契约。配置缺省为空。
- 仅在 `end_session` 将会话状态成功持久化为 `completed` 后触发，并在终态清理前运行。已完成的会话重复调用 end 不重复触发；删除会话不触发。对正在 unwind 的运行，`session_end` 可能先于该 run 的 `run_end`。Hook 是有界同步通知，结束操作最多等待该事件的 5 秒预算；进程在状态提交后、Hook 派发前崩溃时事件可能丢失，不增加 outbox。`run_end` 仍表达每次 Agent run 的结局，与会话终态分开。
- payload 只提供 `event`、`session_id` 和 `success: true`，不传会话正文、摘要或历史。此事件无工具名，matcher 只允许缺省或 `*` 命中。
- Hook 是失败继续的通知，输出不改变 session 状态。其执行沿用现有 5 秒事件预算、64 KiB 输入/输出上限、进程树清理和安全策略撤销。
- 授权按本次结束的 `session_id` 解析执行工作区，并监听该会话的信任撤销。Hook 在 SessionSupervisor 清理会话授权信任前运行；MXC 失败时不回退到宿主进程。
- 诊断将“命令尝试”与“进程已进入隔离边界”分开计数，并只暴露最近问题命令的来源和列表位置，不暴露可执行路径、参数、payload 或进程输出。

## 替代方案

- 复用 `run_end`：拒绝；一次 Agent run 结束不代表用户结束了可继续的会话。
- 在写入 `completed` 前触发：拒绝；持久化失败时可能已对外产生“会话结束”副作用。
- 在会话终态清理之后触发：拒绝；清理会撤销会话授权与执行工作区绑定，Hook 将失去正确的安全上下文。
- 允许 Hook 自动批准工具或用户确认：拒绝；Hook 不替代 Haven 的授权和用户确认机制。

## 影响与迁移

增加一个可选本地配置键和 UI/IPC 字段，不修改数据库、session event 或既有 payload，无需数据库重置。项目配置仍要求用户显式信任精确工作区根。回滚时从 `config.toml` 和受信项目的 `.haven/hooks.toml` 移除 `hooks.session_end`；旧版严格配置解析器不接受该字段。

## 验证

已添加配置解析、诊断序列化、设置 UI 和显式结束去重覆盖。Rust workspace 检查、测试与严格 Clippy，前端检查/测试/构建，以及 IPC 生成漂移检查均已通过。依赖真实 Windows MXC 的边界仍按 ADR 1421 的 Windows 发布验收要求签核。
