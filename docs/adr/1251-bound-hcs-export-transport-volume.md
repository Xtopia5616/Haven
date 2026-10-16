# ADR 1251：为 HCS writable-layer 导出准备有界 transport 卷

## 状态

已实现 broker attempt 私有的有界 transport VHDX owner；尚未接入 `HcsExportLayer`，也尚未在目标 Windows/HCS 主机验证格式、挂载、导出目的路径和 forced-termination 行为。quota runtime gate 继续 fail closed。

## 背景

ADR 1248/1249 已为 HCS layer export 实现有界解码和 stopped writable-layer adapter，但 `HcsExportLayer` 没有输出字节参数。事后限制 decoder 的输入不能阻止同步导出期间写满宿主卷。必须在接入 broker provider 前建立由操作系统实施的传输空间上限。

Microsoft 的 `HcsFormatWritableLayerVhd` 会为容器 writable layer 创建并格式化分区；`AttachVirtualDisk` 可附加 VHD，`HcsGetLayerVhdMountPath` 返回已挂载 HCS writable-layer VHD 的 volume path。`AttachVirtualDisk` 文档要求 `SE_MANAGE_VOLUME_PRIVILEGE`，固定 broker service privilege 集合已包含 `SeManageVolumePrivilege`；HCS formatter 的最小 token 要求仍未在目标机确认。项目已有的 bounded VHDX owner 会在创建后复核 `VirtualSize` 与请求容量完全相等。

## 决定

- 新增私有 `BoundedHcsExportTransport`：从 broker attempt 根目录创建 VHDX，复用 3 MiB–4 GiB、512 字节对齐的容量约束和 `VirtualSize` 复核；由 broker 调用 HCS 格式化，使用 `AttachVirtualDisk` 以无盘符、permanent-lifetime 方式挂载，再用 `HcsGetLayerVhdMountPath` 获取 volume GUID 路径。
- transport 只在新卷根下创建固定 `export` 目录，并将目录 DACL 限定为 SYSTEM、Administrators 与 restricted broker service SID。用于卷子目录的 `DirectoryCapability` 构造器只接受 canonical volume GUID root 和单个普通子目录，并固定目录句柄、拒绝 reparse point。
- `BrokerWorkspaceAttempt` 现在是共享输入、transport 与输出 staging 的唯一 owner 目录；用户 workspace 输入位于其 `workspace-input` 子目录。创建 VHD/挂载前先同步写入非空 active marker。Attempt 的 Drop 和 stale sweeper 都不会清理非空 marker；自动 stale 清理只允许清理旧、未锁定且 marker 为空的 attempt。
- 正常回收顺序为先确认 HCS caller 已返回且 compute system 已停止、关闭卷目录 capability、显式 detach VHD，再把 attempt marker 转为 quiescent 并通过原 capability 删除 VHDX。`BrokerWorkspaceAttempt::cleanup` 拒绝清理非空 marker；只有当前 active marker 能由代码转为空，uncertain marker 不能自动清除。transport Drop 不 detach，未显式 cleanup 的 owner 会保留磁盘与 attempt marker。Detach 失败或 HCS 结果不确定时保留 backing VHDX；调用方必须同时保留 attempt recovery marker。不得让 transport owner 在 HCS worker timeout/cancel 后走普通清理路径。
- 当前切片没有修改 provider/service 的 HCS export 调用。只有目标机验证 HCS 格式化后的 VHDX 可被挂载、HCS export 接受 volume 子目录路径、超额写入被卷容量拒绝，并确认 helper 强制结束后的写入生命周期，才可将此 owner 接入导出链。
- guest storage quota 的运行期 attestation、quota gate 与 `WorkspaceWrite` 授权行为均未改变。传输 VHDX 的最大虚拟容量是候选硬边界，不代表生产 quota 已通过验收。

## 替代方案

- 对导出目录轮询大小或导出后检查：拒绝；同步调用可以在检查间隔内耗尽宿主磁盘。
- 依赖现有 output decoder 的字节上限：拒绝；该上限发生在导出完成之后。
- 对全局宿主卷设置 NTFS 用户配额：拒绝；其作用范围是宿主卷和用户 SID，不提供隔离的 per-attempt 目录容量。

## 影响与迁移

attempt 目录层级调整为 `file-*\workspace-input`，同一父目录可以持有 VHDX 与未来的 output staging。此数据仅为 broker 临时数据，不改变配置、数据库或工具授权，无需重置。若 service 在磁盘挂载或 HCS native operation 期间崩溃，active marker 会阻止自动删除；管理员须先确认 HCS 不再访问该卷、显式 detach 对应 VHDX，再移除 attempt。当前尚无自动 recovery 命令。

## 验证

- Microsoft [`HcsFormatWritableLayerVhd`](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsformatwritablelayervhd)、[`AttachVirtualDisk`](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk) 与 [`HcsGetLayerVhdMountPath`](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsgetlayervhdmountpath) 定义格式化、挂载和 HCS volume path API；具体 transport 导出路径兼容性仍需目标机验证。
- 单元测试覆盖 attempt 子目录结构、active marker crash 保留、volume GUID root 语法拒绝和 VHD owner recovery 保留。没有调用 HCS format/attach/export，也没有 guest 超额写入验收。
- `cargo fmt --package haven-platform -- --check`、`cargo test --locked -p haven-platform` 与 `cargo clippy --locked -p haven-platform -- -D warnings`。

## 回滚

可移除 bounded transport owner 与 volume-child capability constructor，并把 workspace staging 恢复为 attempt 根；配额 gate 必须继续 fail closed。删除任何非空 owner marker 前，须由管理员确认没有仍挂载或仍被 HCS 使用的 VHDX。
