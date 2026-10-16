# ADR 1226：要求 HCS scratch VHD 自身具有硬容量上限

## 状态

已确认设计约束；HCS runtime quota 尚未证明。ADR 1221 的 `StorageSandboxSize` 只能作为 HCS 配置输入，不能单独证明每次执行的 scratch 有硬上限。WorkspaceWrite 继续 fail closed。

## 背景

ADR 1221 构造闭合 HCS V1 JSON，并写入 `StorageSandboxSize`。这证明配置值和字段白名单，不证明后端生成的 writable layer/VHD 最大容量等于该值。微软的 Windows 容器存储文档说明容器默认 C: 虚拟可用空间为 20GB，并允许经 storage-opt 改成较小或较大的值；维护中的 Windows snapshotter 通过复制 `blank.vhdx` 创建每个 snapshot 的 `sandbox.vhdx`，再在请求值非零时调用 `ExpandSandboxSize`。因此仅在 HCS JSON 写入较小 `StorageSandboxSize`，不足以证明默认 scratch 不会保留更大的 VHD 容量。

这一区分影响安全属性：Haven 要求每个 opaque execution 在运行期受 OS 强制的每执行存储上限。配置字段、available-space 采样、输出 manifest 或退出后的 review charge 都不能替代 backing scratch VHD 的容量上限与负向写入证明。

## 决定

- 继续保留 ADR 1220 选定的可选 Windows Container/Hyper-V backend；本 ADR 不改变宿主/镜像许可、显式安装、HCS 不可用时 fail closed、授权与网络隔离 Gate。
- HCS 执行原型必须拥有每次 attempt 的独立 scratch VHD lifecycle。attempt 的有效配额必须与 backing writable VHD 的可见最大容量一致；provider 必须在启动 guest 前验证此关系。仅给 HCS JSON 设置 `StorageSandboxSize` 不满足此条件。
- 如果采用可扩展 VHD，VHD 元数据的最大虚拟容量必须在创建时不超过该 attempt 的已授权配额；实际分配/宿主卷预算仍由有界全局容量 owner 负责。不得把项目 workspace、staging 或其他 host path 以可写形式映射进容器。
- HCS VHD 创建/初始化、挂载、卸载与清理的 owner、所需 Windows privilege、崩溃恢复及 host storage accounting 必须在 quota spike 中单独验证。未经验证，不把 `WindowsContainerExecutionPort` 结果或 mock 当作 quota attestation。
- runtime quota spike 至少证明：guest 对 scratch 的实际写入在配额边界收到磁盘容量错误；未使用任何 host writable mapping；被测 Windows build、Hyper-V isolation、镜像和 quota 值均被记录；HCS system 停止后 attempt scratch 能被回收。配额字段缺失、镜像/host 不匹配、探针错误或 cleanup 不确定时都判失败。
- quota spike 通过之前，不实现 broker IPC/installer、不接入 Shell/Skill/MCP、不调整 WorkspaceWrite 的 opaque execution 拒绝行为。

## 替代方案

- 将 `StorageSandboxSize` 当作自动缩小 scratch 的每次执行硬上限：拒绝；HCS V1 字段本身没有为 Haven 建立 backing VHD 最大容量的证据。
- 只在执行前采样宿主剩余空间或在退出后限制输出：拒绝；两者都不能阻止执行过程中写满 host volume。
- 使用共享可写 host directory / bind mount 作为 scratch：拒绝；容器内部磁盘限制不约束该映射的 host path。

## 影响与迁移

这是对 HCS sandbox 配额实现顺序的约束修正。无数据库、配置、IPC、工具权限或用户数据变化，无需重置。现有 `WindowsContainerSandboxConfig` 只负责构造闭合 HCS 配置，保留其非零字段校验，但其值不再被描述为 quota proof。后续实现须先增加并验证受限 scratch VHD owner，再考虑 broker、staging 和工具接线。

## 验证

- 参考微软 [Windows container storage overview](https://learn.microsoft.com/virtualization/windowscontainers/manage-containers/container-storage) 与 [container resource controls](https://learn.microsoft.com/en-us/virtualization/windowscontainers/manage-containers/resource-controls)。
- 参考 Microsoft [hcsshim HCS V1 schema](https://github.com/microsoft/hcsshim/blob/main/internal/hcs/schema1/schema1.go) 对 `StorageSandboxSize` 的语义，以及 containerd [Windows snapshotter](https://github.com/containerd/containerd/blob/main/plugins/snapshots/windows/windows.go) 对 scratch VHD 的创建与扩容流程。
- 本 ADR 没有改动运行时代码；当前机器无已安装容器镜像，当前进程不是管理员且不在 Hyper-V Administrators 组，故未创建 HCS compute system 或运行负向写入。该环境限制不构成 quota 通过证据。

## 回滚

若后续 Windows spike 证明 HCS writable scratch 无法建立每次执行硬上限，则关闭可选 HCS backend 并回到当前 fail-closed 策略；不得回退为仅依赖 `StorageSandboxSize`、AppContainer TempDir 或宿主 Shell 的 WorkspaceWrite 执行。
