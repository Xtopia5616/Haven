# ADR 1227：按 attempt 配额创建有最大容量的 scratch VHDX

## 状态

已实现 VHDX 创建与容量复核原语；尚未证明 HCS 将该文件用作 guest writable layer。WorkspaceWrite 继续 fail closed。

## 背景

ADR 1226 要求先把每次执行的配额落实到 scratch VHD 本身。Windows `CreateVirtualDisk` 支持在 VHDX 创建参数中设置 `MaximumSize`，`GetVirtualDiskInformation` 可读取创建后的 `VirtualSize`。Haven 还必须将 VHD 文件限定在 capability 持有的私有 scratch 根下，并拥有其清理责任。

官方 VHD API 只证明磁盘文件的虚拟最大容量，不证明 HCS 会把它作为容器的写入盘。containerd Windows snapshotter 采用 `sandbox.vhdx` 文件并可能调用扩容；其具体 writable-layer 初始化和 HCS 消费关系仍需真实 Windows probe 验证。

## 决定

- `haven-platform::windows_container::scratch_vhd::BoundedScratchVhd` 是首个 scratch 磁盘原语。它只接受 3 MiB–4 GiB、512 字节对齐的配额，在已打开的 `DirectoryCapability` 根下生成 `file-*` VHDX。
- 使用 VHDX Version 2 创建参数，将 attempt 配额直接写入 `MaximumSize`；创建成功后调用 `GetVirtualDiskInformation`，并要求返回的最大虚拟容量与配额完全一致。查询错误或不匹配时关闭句柄并尝试经原 capability 删除文件，拒绝返回 owner。
- `BoundedScratchVhd` 持有 capability 和唯一文件名；调用方应在 compute system 停止后显式调用 `cleanup()`，只有成功后才能释放宿主存储预留。`Drop` 只做 best-effort 清理。HCS provider 必须保留该 owner，直到 compute system 停止并确认磁盘可清理；当前类型不持有 compute system 生命周期，也不承担崩溃恢复 owner marker。
- 本原语不格式化 writable layer、不生成 HCS layer metadata、不启动 compute system，不把 capacity getter 当成 HCS quota attestation；也不改变工具授权或 WorkspaceWrite 行为。
- 下一步先实现/验证 HCS writable-layer 格式化、初始化和文件消费顺序，再在目标 Windows、Hyper-V isolation 和产品镜像下做 guest 超额写入及 cleanup probe。只有 guest 返回容量错误且磁盘 owner 完成清理，才满足 ADR 1220/1226 的 runtime Gate。

## 替代方案

- 继续用 `StorageSandboxSize` 推断 scratch VHD 已缩至 attempt 配额：拒绝；字段不能代替 VHD 自身最大容量检查。
- 仅创建 VHD，不读取创建后的容量：拒绝；API 失败或返回值偏差时不能安全进入 HCS。
- 将此 VHD 原语当作完整 sandbox backend：拒绝；它没有证明 HCS 消费该文件、guest 的真实写入边界或崩溃回收。

## 影响与迁移

新增 Windows 平台能力，不修改数据库、持久配置、IPC、授权或工具接线，无需数据重置。VHDX 由动态虚拟磁盘创建，虚拟容量受 attempt 配额限制；实际宿主文件占用、并发总容量预留和 VHD 元数据开销仍须由后续全局容量 owner 处理。

## 验证

- 参考 Microsoft [`CreateVirtualDisk`](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-createvirtualdisk)、[`CREATE_VIRTUAL_DISK_PARAMETERS`](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/ns-virtdisk-create_virtual_disk_parameters) 与 [`GetVirtualDiskInformation`](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-getvirtualdiskinformation) API。
- 参考 containerd [Windows snapshotter](https://github.com/containerd/containerd/blob/main/plugins/snapshots/windows/windows.go) 的每 snapshot `sandbox.vhdx` 创建方式。HCS 格式化 API [`HcsFormatWritableLayerVhd`](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsformatwritablelayervhd) 与 initialize API [`HcsInitializeWritableLayer`](https://learn.microsoft.com/en-us/virtualization/api/hcs/Reference/HcsInitializeWritableLayer) 尚未接入本原语。
- `cargo test --locked -p haven-platform` 在当前 Windows 宿主通过；测试创建 16 MiB VHDX、重新读取准确 `VirtualSize` 并验证 owner 经 capability 显式清理。`cargo clippy --locked -p haven-platform -- -D warnings` 通过。该结果只验证 VHD primitive，不是 HCS 或 guest quota 验收。

## 回滚

若后续 HCS probe 证明此类 VHDX 无法作为 HCS writable layer 或 guest 容量边界不生效，移除此原语并关闭可选 HCS backend；WorkspaceWrite 仍保持 fail closed。
