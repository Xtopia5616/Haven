# ADR 1209：将 OS 强制的 sandbox 存储配额设为 Shell 接线门槛

## 状态

已采纳。`WorkspaceWrite` 下的产品 Shell 继续 fail closed，直到每次 sandbox 执行获得可验证、运行时强制的独立存储上限。

## 背景

ADR 1195–1208 建立了 `ExecutionWorkspace`、私有 `AppContainerWorkspace`、进程树 owner、输出 manifest、pending-review quota 和逐文件晋升。当前 workspace 使用普通系统临时目录。`DirectoryTreeCopyLimits` 限制输入复制，`DirectoryTreeDiffLimits` 限制进程退出后的扫描，review owner 的 512 MiB 限额限制保留的数据；这些都不会限制 AppContainer 进程运行期间写入 Temp 的速度或总量。

只在退出后扫描超限文件并清理不能阻止进程先写满宿主机卷。创建进程前检查剩余磁盘空间也不能阻止并发的其它进程消耗空间。因此，把现有扫描上限或 review 保留限额称为 sandbox 的运行时磁盘配额是不正确的。

当前授权 owner 在 `WorkspaceWrite` 下拒绝 `NetworkAccess::Opaque` 子进程；这个边界包含 Shell、Skill 和 MCP stdio。Shell 只有在 `FullAccess` 下通过现有一次性/策略确认后，才继续使用当前用户的 Windows 权限。review UI 已接线，但在 Shell 生产者完成前不会收到产品 Shell 输出。

## 决定

- `WorkspaceWrite` 下继续拒绝产品 Shell、Skill 与 MCP stdio；不能以“AppContainer 已启动”或“退出后 manifest 有上限”为理由放开。
- 产品子进程接线必须先从 OS-backed storage owner 获得每实例的硬上限；无法在 spawn 前创建和验证该上限时，执行失败关闭。
- 硬上限须覆盖输入快照、baseline、sandbox 中新增与修改的内容，并在 Job Object 整棵进程树归零前保持有效。运行时写满配额应在 sandbox 内返回文件系统错误，不得影响宿主机卷上限。
- active 执行和 pending review 必须共用显式聚合预算。现有 512 MiB review owner 只描述已保留对象的聚合上限；在有 OS-backed owner 前，它不能单独满足 active scratch 的容量契约。
- 保持 `FullAccess` 的既有含义，不通过降低工作区完整性、修改用户卷全局 quota 或隐式切换 sandbox 来替代新模式。

## 替代方案

- **退出后扫描并删除超限输出：拒绝。** 它限制扫描和保留，不限制执行期间写入。
- **用可用磁盘空间检查代替配额：拒绝。** 检查与写入不是原子操作，不能阻止单进程或并发进程耗尽卷空间。
- **启用 NTFS per-user hard quota：暂不采用。** NTFS quota 按文件所有者 SID 聚合到卷，并依赖卷启用 enforcement；Haven 不能为一个短命 AppContainer review 安全地假定用户卷配置，也不能修改全局用户额度。若后续候选能证明独立 SID 计费、受支持的无提升配置和并发语义，可单独复审。
- **在普通用户进程中 attach VHD/VHDX：拒绝。** `AttachVirtualDisk` 文档要求 `SE_MANAGE_VOLUME_PRIVILEGE`；产品不得为临时 Shell workspace 提升权限。
- **采用 experimental `Experimental_CreateProcessInSandbox`：暂不采用。** 当前 Microsoft 文档标注 API 为 experimental；列出的 sandbox specification 没有 storage quota 字段，而且 API 要求 `inheritHandles=FALSE`，与现有经过白名单约束的 stdio pipe launcher 不兼容。
- **持续轮询目录用量并在超限后 kill Job：拒绝作为硬配额。** 轮询存在写入竞态；超限文件可能在检测前占满卷。

## 影响与迁移

本 ADR 不改配置、Tauri IPC、数据库或当前执行行为，无需重置。Shell 在 `WorkspaceWrite` 的已有 fail-closed 状态保持不变；review UI 保留，直到有生产调用方可以安全地产生 review。下一步先评估无需管理员权限、能在进程树生命周期内强制单实例容量的 Windows storage backend，再以一个真实 Windows 负向测试证明超额写入被宿主 OS 拒绝。

## 验证

- `security::tests::opaque_processes_follow_the_selected_network_boundary` 验证 Shell 在 `WorkspaceWrite` 下仍被阻止，在 `FullAccess` 下仍经过确认。
- Windows backend 的验收必须让 sandbox 写满已配置上限，确认下一次写入失败、宿主机卷仍可用、Job 树结束后临时存储被回收；仅检查 manifest 或临时目录大小不算配额验证。
- 审阅 [NTFS disk quota limits](https://learn.microsoft.com/en-us/windows/win32/fileio/disk-quota-limits)、[AttachVirtualDisk](https://learn.microsoft.com/en-us/windows/win32/api/virtdisk/nf-virtdisk-attachvirtualdisk) 和 [Create Process in Sandbox](https://learn.microsoft.com/en-us/windows/win32/secauthz/createprocessinsandbox)。

## 回滚

若 Windows 产品选择了不同的受支持 storage backend，更新本 ADR 与路线图中的实现门槛和验收条件。不得通过删除 fail-closed 授权测试回滚容量保障。
