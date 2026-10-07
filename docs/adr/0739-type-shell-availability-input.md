# ADR 0739：Shell availability 复用 ShellChoice 输入

## 状态

已采纳并实施。

## 背景

`check_shell_available` 唯一 production caller 位于 Settings 页面，按 `cmd`、`powershell`、`pwsh` 检查 Shell 设置选项。这三项已由 Common `ShellChoice` 表达并用于 `Settings.default_shell`，前端也已有生成的 `ShellChoiceInput`。

App handler 原先接受开放 `String`。Windows 上仅对 `pwsh` 查询 PATH，其它任何字符串都返回 available；非 Windows 也对未识别名称默认返回 available。页面不会发送这些未知值，但它们仍是可调用的 IPC 输入。

## 决定

- `check_shell_available.shell` 接受 Common `ShellChoice`，生成到 `ShellChoiceInput`；Settings 页面诊断循环使用 `SHELL_CHOICE_INPUT_VALUES`，并把 shell 可用性 map 与选项绑定到生成的 choice union。
- Handler 按 `ShellChoice` 做穷尽的平台匹配。对三个有效 shell 的可用性语义保持不变；空字符串与未知 shell 在 Tauri 反序列化边界拒绝。
- 不新增 App 级或 UI 级 shell enum；默认配置与诊断命令共享已有 Common 领域词汇。

## 替代方案

- 保留开放 `String` 并对未知值继续返回 available：拒绝。该值不是任意可执行文件探测接口，Settings 只诊断明确支持的 shell 选项。
- 再定义一个 diagnostics 专属 enum：拒绝。它会与 `Settings.default_shell` 重复表达同一组值，且没有独立生命周期或消费者。

## 影响与验证

Generated command request 从 `shell: string` 收窄为 `shell: ShellChoiceInput`。三个已支持 shell 的 IPC 文本与平台检测行为不变；非法 shell 不再获得误导性的 available 响应。无配置字段、持久化数据、schema、迁移或重置变化。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked` 通过；UI `check`、`test:run`（125 files / 992 tests）、`build` 通过；IPC command contract 检查（80 handlers）、IPC event 检查（35 channels）、ADR index（722 records）及 `git diff --check` 通过。

## 回滚

如回滚，恢复 handler 的 `String` 参数及开放 shell 请求类型，Settings 诊断循环可恢复固定字面量数组；无需修改 Common `ShellChoice`、配置文件或重置数据。
