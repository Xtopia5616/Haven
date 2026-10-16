# ADR 1245：通过有界差异流重建 broker 执行输出树

## 状态

已实现 broker 输出差异的私有 sender/receiver：发送端只传 bounded manifest changes 与变化后文件的 32 KiB 数据块；接收端将数据写入独立 capability-rooted staging，从私有 baseline 重建完整输出树，并重新计算 manifest 后才返回结果。现有协议 v2 已预留 `OutputEntry`、`OutputFileChunk` 和 `OutputSnapshotComplete`，本 ADR 为其增加实现。named-pipe/service/provider/Tools 尚未调用这些函数；HCS writable-layer 到普通文件树的提取仍未实现，quota gate 继续 fail closed。

## 背景

broker 必须将 guest 产生的文件变化交给 Haven 的 `SandboxOutputReview`，但不能把 HCS layer 目录或 VHDX 直接解释成普通目录。Microsoft 将 `HcsExportLayer` 定义为容器 layer 的传输导出 API；hcsshim 的 Windows layer exporter 会先激活并准备 layer，再以 layer reader 逐条导出文件。HCS provider 仍须独立完成并验证从 writable layer 到 capability-rooted 输出目录的提取，本 ADR 只定义该目录之后的 broker 传输与接收重建边界。

接收端如果只信任 broker 发送的 manifest，可能接受被篡改或不完整的文件数据；如果只收变化文件而不重建最终树，也无法复用现有 `DirectoryCapability::output_manifest` 和 `SandboxOutputReview` 语义。

## 决定

- 输出 sender 接收已停止且不可变的 output tree、其 baseline、caller diff limits 和初始 snapshot byte count。它计算 manifest，按连续 index 发送 `OutputEntry`；对 after kind 为 file 的条目发送带 little-endian index header 的 32 KiB `OutputFileChunk`。发送中复核文件长度与 SHA-256，发送前后重扫 manifest；源在传输期间改变则不发送完成帧。
- Receiver 只能写入 attempt 私有的两个空 capability：完整最终树 staging 与变化文件 staging。它先将 baseline 按 caller copy limits 复制到最终树；每条变化在接收文件 payload 前验证顺序、路径深度、单文件大小与 changed-total 上限；payload 长度和 SHA-256 不匹配时失败。
- 收到 `OutputSnapshotComplete` 后，receiver 按路径深度先删后建，将变化文件应用于私有树，清空 chunk staging，并重新计算 output manifest。只有 entry count、初始快照字节、changed byte count 和每条 manifest change 都与传输元数据一致才成功返回完整树和 manifest。
- 传输、取消、IO、摘要或重建任一步失败时，调用方必须丢弃整个 attempt staging；不得把部分输出交给 review owner。sender 与 receiver 都不授权晋升，不持久化绝对路径，不替代 named-pipe peer authentication、policy revision 检查或 quota attestation。
- 每文件最多 64 MiB、总变化最多 256 MiB、最多 4096 个变化沿用 `DirectoryTreeDiffLimits` 的硬上限；每二进制帧最多 64 KiB，输出文件 payload 使用 32 KiB chunk 以容纳 index header。

## 替代方案

- 只传 manifest、让 client 再从 broker 路径读取：拒绝，broker 私有绝对路径不能越过 IPC，且生命周期与权限边界错误。
- 直接把完整输出目录作为 host writable mapping：拒绝，会让 guest 绕过 broker staging 和输出审阅边界。
- 先将文件内容无界缓存在内存中再发：拒绝，变化数据必须在执行 limits 内流式传输。
- 仅信任 broker 的变化摘要，不在接收端重建并复算：拒绝，无法证明传输 payload 对应于交付给 review 的最终树。

## 影响与迁移

无 schema、权限 key 或协议版本变化；协议 v2 已定义对应 control message 与 output chunk frame。本实现只在 platform 内新增私有模块，不接入执行路径。HCS layer 导出、attempt owner 的 crash cleanup、named-pipe response loop、`SandboxOutputReview` owner construction 和 Tools pending-review 接线仍是后续任务。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- 本地 capability directory 与内存帧测试覆盖完整差异重建、大文件 chunk、创建/修改/删除、文件目录类型切换、嵌套删除和 payload digest 篡改。
- 未启动 HCS、named pipe 或 guest；未证明 HCS writable layer quota、layer export、compute-system 生命周期或生产 service 的取消/清理。quota runtime attestation 仍是生产执行硬门槛。
- HCS layer 导出契约参考 Microsoft [HcsExportLayer](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsexportlayer) 与 Microsoft-maintained [hcsshim Windows layer exporter](https://github.com/microsoft/hcsshim/blob/main/pkg/ociwclayer/export.go)。

## 回滚

移除私有 output-transfer module、对应测试及本 ADR/路线图引用即可回滚。不得以 host writable mapping 或无界文件缓存替代。
