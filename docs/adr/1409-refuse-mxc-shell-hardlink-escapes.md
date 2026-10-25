# ADR 1409：拒绝 MXC Shell 写根中的 hardlink 越界

## 状态

已采纳并实施。Windows BaseContainer 真机验收通过；AppContainer+DACL ACL 恢复仍需在没有 BaseContainer 的 MXC 主机上验收。

## 背景

ADR 1404 将 Shell 的有效读写根编译为 MXC ProcessContainer 的目录策略。真机验收确认 junction 指向根外时写入会被 MXC 拒绝，但发现另一条别名路径：在授权写根中预先放置一个指向根外文件的 NTFS hardlink 后，BaseContainer Shell 可以通过该名字修改根外目标。hardlink 是同一文件对象的第二个目录项，目录根 ACL 无法区分这两个名字。

## 决定

1. 在 Shell 的 MXC 文件策略编译阶段，逐个检查最终有效的可写目录根。使用 `DirectoryCapability` 持有的目录句柄递归检查普通文件，并通过打开后的文件句柄读取 hardlink 数量；不跟随 symlink 或其它 reparse point。
2. 任一可写根含有多链接文件时，在 MXC spawn 前拒绝本次 Shell。若目录读取、文件打开、链接数查询失败，或遍历超过每根 1,000,000 个条目，也拒绝执行。
3. 检查覆盖 workspace、一次性确认的扩展目录、scratch 与被授权的包缓存写根。显式只读和拒绝策略仍由原文件策略编译流程实施。
4. 不改变配置、数据库或 IPC。没有安全的 MXC 文件对象级边界前，不允许 Shell 对包含 hardlink 的写根降级或跳过检查。

## 替代方案

- 只依赖 MXC 的目录根 ACL：拒绝，真机复现证明 hardlink 名称可绕过根外文件的路径边界。
- 自动复制或拆分 hardlink：拒绝，这会在 Shell 运行前改写用户文件身份或内容。
- 只在测试中拒绝 hardlink：拒绝，生产 Shell 仍会保留已复现的越界写入。
- 对检查失败或超大目录继续运行：拒绝，无法证明 MXC 写根没有 hardlink 别名。

## 影响与回滚

Shell 每次运行会对最终可写根执行有界递归检查。包含 hardlink 的目录树或超过检查预算的树会 fail closed；用户可移除 hardlink 或缩小 Shell 写入根后重试。检查不修改目录内容、权限或 ACL。

移除检查会重新开放已由真机复现的 hardlink 越界写入；只有当 MXC 能按文件对象身份执行写边界并通过对应 Windows 验收后，才可重新评估。

## 验证

- Windows build 29683 的 MXC BaseContainer 真机用例复现预置 hardlink 越界写入；加入写根检查后，Shell 在 spawn 前拒绝执行，外部目标保持原内容。
- 同一主机验证 MXC 拒绝从写根创建指向根外文件的 hardlink，也拒绝从只读子路径创建可写别名；junction 越界写入继续被拒绝。
- `cargo test --locked -p haven-platform` 通过（66 项），包括普通文件、预置 hardlink 拒绝及扫描预算 fail-closed；`cargo test --workspace --locked` 与 `cargo fmt --all -- --check` 通过。
- 严格 Clippy 未通过：workspace 检查在 `haven-common`、`haven-messaging` 与 `haven-memory` 的 lint 处失败；限定 `haven-platform` / `haven-tools` 的检查也发现现有工具 crate lint。该门禁未计为通过。
- 当前主机只探测并使用 BaseContainer，`sandbox_notice` 确认本次没有 DACL 修改；此结果不证明 AppContainer+DACL 的 ACL 恢复。该路径仍是 Windows 发布验收 Gate。
