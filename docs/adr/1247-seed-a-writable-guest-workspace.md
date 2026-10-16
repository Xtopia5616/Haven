# ADR 1247：在容器 writable layer 中建立 guest workspace 副本

## 状态

已实现固定 seed process 参数与 writable guest cwd 的契约测试；尚未由 provider 调用 seed process，也未在 HCS guest 中验证 robocopy 行为。broker quota gate 继续 fail closed。

## 背景

ADR 1237 将 broker 私有 workspace 快照只读映射到固定 guest 路径。先前 `HcsGuestProcessParameters` 又将用户 Shell 的 `WorkingDirectory` 放在该只读映射下，因而 guest 文件写入无法进入 HCS scratch writable layer，也无法通过该层的 quota 限制和输出 review 回传。

## 决定

- 固定只读输入路径为 `C:\Haven\workspace-input`，固定可写工作副本根为 `C:\Haven\workspace-output`。用户 cwd 仍按 workspace-relative 路径验证，并映射到后者。
- 新增不包含用户命令内容的固定 seed process 参数：以 restricted PowerShell 调用系统 `robocopy.exe`，将只读输入复制到 writable layer；拒绝已存在的目标目录，复制包括空目录，使用数据/属性/时间戳复制模式并跳过 junction。Robocopy 的 0–7 结果映射为成功，8 及以上映射为 seed 失败。
- seed 与用户 Shell 使用分开的 HCS process 参数；两者关闭 stdin 并启用有界 stdout/stderr pipe。用户 Shell 只有在 seed process 确认成功后才能启动。
- provider 尚未创建；在 seed 调用、guest cwd 和后续输出提取接入之前，不执行请求命令、不改变 quota gate 状态。

## 替代方案

- 继续把用户 Shell 放在只读 mapped directory：拒绝；这使 WorkspaceWrite 语义不可用。
- 将 workspace snapshot 以可写 host directory map 提供给 guest：拒绝；这会绕过 attempt VHD 的 OS 存储配额，并直接暴露 broker host 文件系统。
- 将用户请求文本拼入 seed 脚本：拒绝；bootstrap 的程序、路径和选项固定，用户命令只存在于后续独立 Shell process 参数中。

## 影响与迁移

该变化只调整未接入生产执行的 HCS guest 参数，不改变数据库、用户配置、IPC 或持久数据，不需要重置。测试版本无需兼容旧 guest cwd。

## 验证

- crate 门禁通过：`cargo fmt --package haven-platform -- --check`、`cargo check --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings`；`cargo test --locked -p haven-platform` 为 112 passed、1 ignored。
- 单测解码 seed process 的 UTF-16LE Base64 命令，验证 source/destination 是固定路径、目标存在时失败、Robocopy 错误阈值、只读 source/writable cwd 分离，并确认 JSON 不包含用户 Shell 命令。
- 没有启动 HCS guest；静态参数与单测不证明实际镜像包含 PowerShell/Robocopy，也不证明复制和 storage quota 行为。

## 回滚

provider 未接线前可移除 seed 参数 builder 与 cwd 路径变更。任何接线若无法证明输入仍为只读、工作副本位于 quota-bound VHD，或 seed 失败时不会启动用户命令，必须保持 HCS backend unavailable；不得回退到可写 host mapping。
