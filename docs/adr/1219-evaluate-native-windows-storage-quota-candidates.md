# ADR 1219：评估 Windows 原生 sandbox 存储配额候选

## 状态

评估完成。当前 Windows 原生 sandbox API、NTFS 用户配额和普通用户 VHD attach 不能单独满足每次执行的 OS 强制存储硬上限；可选 Windows Containers/HCS 部署由 ADR 1220 定义。

## 背景

ADR 1209 要求在 `WorkspaceWrite` 下接通 Shell、Skill 或 MCP stdio 前，必须有运行期存储硬配额。复核 Windows 11 `Experimental_CreateProcessInSandbox`、NTFS 用户配额和 VHD attach API，判断这些方案是否能替代 Windows Containers/HCS 的独立存储 owner。

## 决定

- 不把 Windows 11 `Experimental_CreateProcessInSandbox` 当作存储配额后端。Microsoft 将 API 标为 experimental；当前公开 sandbox specification 有 AppContainer、路径授权、网络和 UI 限制字段，但没有每执行磁盘容量字段。
- 不在 Haven 启动时修改 NTFS 卷级 quota 状态或把 per-user quota 用作 sandbox scratch 配额。NTFS quota 是系统管理员配置的卷级状态，默认 enforcement 关闭，也不能为一次执行建立独立生命周期 owner。
- 不用 VHD/VHDX attach 作为普通用户执行路径。Windows `AttachVirtualDisk` 要求 `SE_MANAGE_VOLUME_PRIVILEGE`；该方案仍需额外定义磁盘格式化、挂载身份、崩溃清理和宿主可用空间边界。
- 不改变 `WorkspaceWrite` 下 opaque 子进程的拒绝行为；容器隔离、路径 ACL、manifest/输出上限、磁盘空间轮询或终止后清理都不能代替运行期间的存储容量强制。
- HCS 是独立的可选后端决策，不由本 ADR 推导；其宿主、服务、许可和 fail-closed 边界见 ADR 1220。

## 替代方案

- 使用 experimental sandbox API 并假设它隐式限制磁盘写入：拒绝；公开字段没有容量限额语义。
- 使用 NTFS per-user quota：拒绝作为每执行后端；卷 enforcement 依赖管理员配置，且可能影响同 SID 在同卷的其它文件。
- 每次执行创建并 attach VHDX：拒绝作为普通用户路径；挂载需要 volume-management privilege，并增加持久磁盘生命周期状态。
- 将 quota 变成软预算并轮询剩余磁盘：拒绝；检测竞态仍可能让进程写满宿主卷。

## 影响与迁移

本 ADR 只完成原生候选评估，不改代码、配置、IPC、数据库或持久字段，无需重置。其初始的 no-admin 评估前提已由 ADR 1220 明确调整为可选、显式安装的特权 Windows 容器服务部署；不适用该部署的环境继续 fail closed。

## 验证

- Microsoft [Create Process in Sandbox](https://learn.microsoft.com/en-us/windows/win32/secauthz/createprocessinsandbox) 将 API 标为 experimental；当前公开 sandbox specification 不含磁盘 quota/capacity 限额。
- Microsoft [System-level Administration of Disk Quotas](https://learn.microsoft.com/en-us/windows/win32/fileio/system-level-administration-of-disk-quotas) 将配额配置和 enforcement 定义为系统管理员对 volume 的 per-user 管理，并说明默认 quota 状态为 disabled。
- Microsoft [`AttachVirtualDisk`](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk) 文档要求调用者具备 `SE_MANAGE_VOLUME_PRIVILEGE`。
- 本 ADR 不关闭 ADR 1209 的运行期超额写入 Windows 验收 Gate。

## 回滚

未来原生 Windows API 若提供受支持的 per-execution 存储硬上限，可新增 ADR 复核其服务权限、workspace 绑定、超限行为、崩溃恢复和 Windows 负向验收；删除本评估不会自动放开 `WorkspaceWrite` 子进程。
