# ADR 1230：按 Hyper-V 容器镜像协议验证 scratch VHD

## 状态

已记录源码验证结果；未在受支持且具备 HCS 权限的 Windows 环境中运行。HCS quota runtime gate 仍未通过，`WorkspaceWrite` 继续 fail closed。

## 背景

ADR 1226–1229 已创建带 `MaximumSize` 的空 VHDX，并实现 HCS writable-layer 格式化原型，但尚未证明该文件可以作为 Hyper-V 容器的实际 scratch 启动。

本轮核对微软 HCS API 与当前 containerd/hcsshim 实现后发现，需要区分两种存储流程：

- containerd 当前 Windows snapshotter 的 Hyper-V scratch 路径，从最后一个父镜像层复制其 `blank.vhdx` 到本次 snapshot 目录，命名为 `sandbox.vhdx`；配置了 scratch size 时再调用 `ExpandSandboxSize`。该路径没有调用 `HcsInitializeWritableLayer`。
- `HcsInitializeWritableLayer` 的 API 契约是根据 antecedent layer JSON 初始化一个 writable layer；它是 HCS storage utility API，但官方 API 目录和 hcsshim wrapper 本身不能证明它是 Hyper-V `sandbox.vhdx` 创建流程的一步。
- 当前 hcsshim 的 `FormatWritableLayerVhd` 注释记录了操作系统句柄契约变化：Windows Server 2019 要求通过 `CreateFile` 得到的 VHD 文件句柄；更新系统要求 Virtual Disk API 返回的句柄。当前 Haven 原型走 Virtual Disk API，不能据此宣称覆盖 Server 2019。

因此，“VHDX 元数据容量准确”与“该 VHDX 可作为目标镜像的 Hyper-V scratch”是两个独立命题。未经启动和 guest 写入探针验证，不能把当前新建空 VHDX 接到 compute system。

## 决定

- 将目标契约限定为 ADR 1220 选定的 Hyper-V isolated Windows Container。执行 attempt 使用自己的 `sandbox.vhdx`，不映射任何可写 host path。
- 在产品镜像和受支持的目标 Windows build 上验证完整的 scratch 消费协议：父镜像层及其 `blank.vhdx` 的要求、scratch 的路径/层配置、HCS 创建与启动、guest 文件系统实际挂载、超过 attempt 配额时的磁盘容量错误，以及 HCS 停止后清理成功。
- 继续将 `HcsInitializeWritableLayer` 视为待证明的候选 API。只有实验能证明所选 Hyper-V 配置需要它时，才实现并接入该调用；不以 API 名称相似或文档存在作为使用依据。
- 在现有空白 VHDX 原型与镜像提供的 `blank.vhdx` 模板路径之间作出实现选择前，先取得能运行 HCS 的验证环境和产品兼容镜像，并记录 Windows build、隔离类型、镜像 digest、VHD 格式/最大虚拟容量及所有相关 HCS 参数。不得根据静态源码分析宣布 quota gate 通过。
- 记录 `HcsFormatWritableLayerVhd` 的 OS 句柄差异。未来如扩大支持矩阵到 Windows Server 2019，必须提供独立 API adapter 与对应实机验证；未验证的 OS/backend 组合 fail closed。
- 本 ADR 不增加 broker IPC、service installer、HCS provider 或工具接线；在 runtime quota gate 通过前，scratch 原语继续 crate-private，Shell/Skill/MCP 的 WorkspaceWrite opaque execution 继续拒绝。

## 替代方案

- 仅因 `HcsInitializeWritableLayer` 名称包含 writable layer，就将它接入 Hyper-V scratch 路径：拒绝；当前 containerd Hyper-V scratch 构造路径没有此调用。
- 将 `HcsFormatWritableLayerVhd` 成功等同于 scratch 已被 HCS 挂载并受 guest 配额限制：拒绝；格式化只证明分区准备，不证明 compute-system 消费与超额写入行为。
- 直接复制镜像 `blank.vhdx` 并用 `ExpandSandboxSize` 接受指定值作为 quota：拒绝；该 API/路径在当前源码中用于扩容，尚未证明其能缩小至每次执行的硬容量上限。

## 影响与迁移

仅细化未接线的 HCS spike 验收顺序；无数据库、配置、IPC、授权或用户数据变化，无需重置。现有 VHD 创建/容量/cleanup primitive 保留为实验材料，但不代表 Hyper-V scratch 已可用。

## 验证

- Microsoft [`HcsInitializeWritableLayer`](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsinitializewritablelayer) 说明该 API 接收 writable layer 目录和 antecedent layers JSON；Microsoft [HCS API overview](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/apioverview) 将其列为 storage utility。
- 当前 [containerd Windows snapshotter](https://github.com/containerd/containerd/blob/main/plugins/snapshots/windows/windows.go) 的 `createScratchLayer` 复制父层 `blank.vhdx` 为 `sandbox.vhdx`，按需调用 `ExpandSandboxSize`；该实现未调用 `HcsInitializeWritableLayer`。
- 当前 [hcsshim `FormatWritableLayerVhd`](https://github.com/microsoft/hcsshim/blob/main/computestorage/format.go) 文档注释记录 Server 2019 与更新系统的句柄差异；其 [InitializeWritableLayer wrapper](https://github.com/microsoft/hcsshim/blob/main/computestorage/initialize.go) 将父只读层 metadata 传给 HCS。
- 当前环境尚无可用容器镜像，进程 token 也未取得 HCS formatter 所需权限；本轮未尝试启动 compute system 或 guest 超额写入。源码核对不是 runtime 验收。

## 回滚

若目标 Windows/HCS 无法提供可验证的每执行硬容量 scratch，关闭可选 HCS backend，保留现有 WorkspaceWrite fail-closed 行为；不得退回共享可写 host path、仅靠 `StorageSandboxSize` 或仅靠退出后的输出限制。
