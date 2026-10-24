# ADR 1391：为 WorkspaceWrite Shell 增加 MXC 兼容隔离 tier

## 状态

已采纳并实施。

## 背景

ADR 1368 将 `WorkspaceWrite` Shell 接入 MXC ProcessContainer。当前代码要求 probe 必须返回 `base-container` 且不需要 DACL 补强，因此即使 MXC 自身已选择可用的 AppContainer tier，也会在调用前拒绝 Shell。结果是较新 Windows 的 BaseContainer 能力成为 WorkspaceWrite 的单点条件。

## 决定

- 先按原始请求 probe BaseContainer。只有完整策略可由 BaseContainer 执行时，才使用原始网络和 UI 策略启动。
- 原策略 probe API 报错、BaseContainer 无法执行原策略或原策略 spawn 失败时，继续按有序策略重试；只接受 probe 明确报告且无错误的 BaseContainer、AppContainer+BFS 或 AppContainer+DACL。原策略本来就拒绝网络时，可直接采用语义相同的 AppContainer tier。MXC 启动失败后继续下一档时，依赖 MXC runner 在返回错误前终止/回收尚未成功启动的子进程。
- 兼容 tier 下若原请求允许网络，将网络收窄为仅允许公网出站连接，并拒绝私有网络入站和 host loopback。原请求拒绝网络时继续全拒绝。其它文件范围、工作目录、环境和进程生命周期边界保持原授权值。
- 若仅公网出站兼容策略仍无法 probe 或启动，则再试完全断网的兼容策略；结果必须显式告知 Shell 调用者及模型网络已关闭。所有策略均失败时报告各档失败阶段和原因，不回退为普通宿主进程。
- AppContainer+DACL tier 可在授权路径上临时调整 ACL，MXC 在进程结束时恢复。Shell 结果显示当前兼容 tier、ACL 临时调整及网络降级说明；日志记录 tier 和 probe warnings。
- MXC probe/spawn 错误显示发生阶段及稳定原因类别（由 SDK `ErrorCode` 映射）；MXC 错误消息与 remediation 在进入 Shell 错误前脱敏，operation/native code 保留在受控诊断日志。
- MXC 兼容成功时 probe 与运行时 warnings 显示在 `sandbox_notice`；失败回退期间已观察到的 warnings 会保留到后续兼容提示或最终错误，不因 spawn 失败丢失。已知的 `prepare-system-drive` 和 `prepare-null-device` warning 映射到稳定分类，并给出管理员准备命令；未知 warning 有界显示且路径脱敏。模型工具描述要求向用户解释该提示，特别是网络权限降级和主机准备操作。准备命令及其语义遵循 [MXC host preparation](https://github.com/microsoft/mxc/blob/main/docs/host-prep.md)。
- 不新增设置项、配置字段、IPC 命令或数据迁移。

## 替代方案

- 只接受 BaseContainer：拒绝，会继续使一批具备 AppContainer 支持但没有新 BaseContainer 合同的 Windows 主机无法使用 WorkspaceWrite Shell。
- 退回当前用户权限的普通 host process：拒绝，会使工作区写入范围失效。
- 用 AppContainer 兼容 tier 保留原有入站/host-loopback 网络：拒绝，MXC 的兼容 tier 无法完整执行这项策略，因此将网络缩为仅允许公网出站。

## 影响与回滚

Shell 的 `WorkspaceWrite` 在兼容 tier 上可用范围扩大；兼容 tier 的文件隔离较 BaseContainer 弱，DACL 可能短暂变化，网络权限收窄为仅出站。调用结果会明确显示该降级。

回滚时恢复仅允许 BaseContainer 的检查，并删除兼容请求与提示字段；不改变 `FullAccess`、`ReadOnly` 或其他进程入口。

## 验证

- 单元测试覆盖兼容 tier 判定、初次 probe API 错误后的重试、仅公网出站到全断网的降级顺序、策略拒绝、MXC 错误分类与脱敏、系统盘/NUL 主机准备提示及有界警告输出。
- UI 测试覆盖 Shell 结果卡片显示降级说明。
- Windows 真实 MXC 验收需分别覆盖 BaseContainer 和 AppContainer+DACL 主机；当前开发机的 BaseContainer 路径继续由既有 Windows Shell/MXC 验收覆盖。若当前主机只提供 BaseContainer，则 AppContainer+DACL 真机路径记为环境受限，不据此宣称已运行验收。
