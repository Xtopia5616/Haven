# ADR 1252：在隔离 helper 中验证有界 HCS export transport

## 状态

已实现 worker plan 对 bounded transport owner 的引用，以及 helper 对 VHDX 容量和挂载 volume 身份的重新验证；尚未接入 service/provider，也未调用真实 HCS export API。Quota runtime gate 继续 fail closed。

## 背景

ADR 1250 将同步 `HcsExportLayer` 放进隔离 helper，ADR 1251 为 export transport 建立有界 VHDX owner。只把导出目录路径传给 helper，无法证明该目录属于有容量上限的 backing VHDX；helper 必须在 native export 前独立验证这层关联。

Microsoft [`GetVirtualDiskInformation`](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-getvirtualdiskinformation) 要求查询句柄通过 `VIRTUAL_DISK_ACCESS_GET_INFO` 打开；[`HcsGetLayerVhdMountPath`](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsgetlayervhdmountpath) 返回 mounted virtual disk 的 volume path，并要求用 `LocalFree` 释放返回值。Helper 重新打开已挂载 VHDX 后再调用 HCS path API 的兼容性仍须在目标 Windows build 验证。

## 决定

- 生产 `HcsLayerExportPlan` 只能由 `BoundedHcsExportTransport` 构造，并将 backing VHDX 路径与最大虚拟容量写入 version 2 worker request。测试专用 plain-directory constructor 不进入非测试构建。
- helper 重新打开 VHDX，仅请求虚拟磁盘信息权限，校验其 `VirtualSize` 与请求容量相等且容量符合既有 3 MiB–4 GiB sector-aligned 范围；拒绝无效、零长度或重解析路径上的 backing 文件。
- backing VHDX 的父目录也由 `DirectoryCapability` 固定，并参与 transport、writable layer、output 与 parent-layer 根目录的互斥检查，避免导出源/目标共享宿主目录。
- helper 调用 `HcsGetLayerVhdMountPath` 重新取得该 VHDX 的 volume GUID 路径，并要求 request 的 export 路径与该卷根下固定的 `export` 子目录一致；Windows 路径大小写按不敏感规则比较。目录最终仅通过 `DirectoryCapability::open_hcs_volume_child` 打开。
- 缺失或不成对的 VHDX 路径/容量、容量不匹配、volume GUID 不匹配、目录无法固定或目录非空时均在加载/调用 HCS export API 前失败关闭。
- 该验证只连接 owner 与 helper 的协议契约；service/provider 尚未调用 plan，亦不构成实际 HCS export 路径兼容或 guest quota 运行期证明。取消/超时后的 transport 必须继续按 ADR 1250/1251 保留；quota gate 与 `WorkspaceWrite` 授权行为不变。

## 替代方案

- 信任父进程传入的 export 目录路径：拒绝；worker 应在自身权限边界内核实 directory 对应于哪个已挂载 VHDX。
- 只检查 export 目录的 volume GUID 语法：拒绝；语法不能证明目录所处卷来自本次 bounded transport。
- 在 worker 中重新 attach VHDX：拒绝；owner 已以 permanent lifetime 挂载磁盘，重复 attach 会扩大生命周期竞态。

## 影响与迁移

私有 worker request 升为 version 2，严格传递 bounded transport backing path 与 capacity。该协议仅在 broker 与其同版本私有 helper 之间使用，不影响 named-pipe 协议、持久数据、用户配置或数据库，无需重置。测试仍可使用合成目录，但仅在 `cfg(test)` 下接受缺少 transport identity 的 request。

## 验证

- 平台 crate 门禁：`cargo fmt --package haven-platform -- --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings` 与 ADR index checker。
- 单测验证 worker version/ framing/尺寸/层重叠及 transport 身份字段必须成对出现。没有运行 HCS formatter、mount、export 或 guest 超额写入验收。

## 回滚

可回退 worker request 到 version 1，并移除 bounded transport identity 验证；任何生产接线仍须保持关闭，直到替代方案能证明 HCS export 只能写入受 OS 容量上限保护的 transport 卷。
