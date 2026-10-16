# ADR 1242：从 typed Shell request 构造闭合 HCS guest process 参数

## 状态

已实现 crate 私有 `ProcessParameters` builder 与构造/边界单测；尚未调用 `HcsCreateProcess`。guest cwd 固定在只读 workspace 输入映射，因此 broker 当前不会创建或运行 guest command。quota gate 与生产执行状态不变。

## 背景

ADR 1241 为 compute-system 提供了有界 handle 生命周期，但 guest 进程启动仍没有参数边界。HCS `ProcessParameters` 包含可选择的程序、命令行、工作目录、环境、token 与标准流选项；若 broker 直接序列化 IPC 内容或继承服务环境，会把执行面扩大到 typed Shell request 之外。当前 HCS 只提供只读 workspace 输入映射，没有安全的 guest writable output layer。

## 决定

- builder 只接受 `WindowsContainerExecutionRequest`，应用路径固定为 Windows 自带 `cmd.exe` 或 Windows PowerShell 5；不会接受调用者程序路径、任意参数 JSON、环境变量或 guest 绝对路径。
- HCS cwd 固定为 `C:\Haven\workspace-input` 下经校验的相对目录。路径组件拒绝 traversal、重解析别名和 Windows 非法/保留名称；cwd 不存在时由 HCS 创建进程失败处理，不在 host 上回退。
- 环境是 6 个固定 Windows 运行变量，不读取或合并 broker/用户环境。使用 `RestrictedToken=true`、关闭 stdin、分别创建 stdout/stderr pipes、关闭 console emulation 与 existing-login 行为。
- `cmd.exe` 参数按 Windows CreateProcess 命令行规则 quoting；PowerShell command 转成 UTF-16LE Base64，并通过 `-EncodedCommand` 传入，避免命令文本破坏参数边界。Shell command 仍是已获授权的用户代码，builder 不替代上游 permission/sandbox 检查。
- 生成的完整 UTF-16 命令行限制在 Windows 进程命令行上限内。所有 JSON 由固定结构序列化；HCS ProcessParameters 字段契约参见 Microsoft [JSON schema reference](https://learn.microsoft.com/en-us/virtualization/api/hcs/schemareference#processparameters)。
- builder 的 cwd 对应只读输入层，因此此阶段不启动 guest process。接线必须等安全的 writable layer/output review 和 quota runtime attestation 完成后，继续复用 Windows Container Tools authorization gateway；不把可写 host directory 映射到 guest。

## 替代方案

- 直接把 request 字段拼进 HCS JSON：拒绝，未来 IPC 增字段时会意外开放程序、环境或路径控制。
- 继承 broker 服务环境：拒绝，服务账户变量和安装路径不应自动成为 guest command 环境。
- 把输出直接写进普通 broker host directory，再事后限制 manifest：拒绝，运行时仍可能耗尽宿主机卷。
- 在本 ADR 中调用 guest process 并假设只读工作区够用：拒绝，命令的输出目录、review 与 storage enforcement 尚未形成安全链。

## 影响与迁移

新增 Windows-only 私有进程参数 builder 和 `base64` 已锁定依赖；不改变 named-pipe 协议、数据库、Tauri IPC 或用户设置。当前 service 不使用该 builder，现有 broker 行为无变化。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- 单测覆盖固定可执行程序/环境/token/pipe 标志、工作目录限制、PowerShell Unicode 命令的 UTF-16LE Base64 往返，以及最终 Windows command line 长度上限。
- 未调用 `HcsCreateProcess`，未运行真实 guest command，未安装或启动 broker 服务，未证明写入 quota。

## 回滚

移除私有 process parameter builder、平台 crate 对 `base64` 的依赖与本 ADR 路线图引用即可回滚；当前 broker 不会启动此 builder 创建的 process，不需要清理持久状态。不得在回滚时把 user command 或服务环境直接序列化为 HCS 参数。
