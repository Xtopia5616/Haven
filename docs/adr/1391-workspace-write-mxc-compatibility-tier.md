# ADR 1391：为 WorkspaceWrite Shell 增加 MXC 兼容隔离 tier

## 状态

已采纳并实施。

## 背景

ADR 1368 将 `WorkspaceWrite` Shell 接入 MXC ProcessContainer。当前代码要求 probe 必须返回 `base-container` 且不需要 DACL 补强，因此即使 MXC 自身已选择可用的 AppContainer tier，也会在调用前拒绝 Shell。结果是较新 Windows 的 BaseContainer 能力成为 WorkspaceWrite 的单点条件。

## 决定

- 先按原始请求 probe BaseContainer。只有完整策略可由 BaseContainer 执行时，才使用原始网络和 UI 策略启动。
- BaseContainer 无法执行原策略时，使用收窄网络权限的兼容请求重探；只接受 probe 明确报告且无错误的 BaseContainer、AppContainer+BFS 或 AppContainer+DACL。兼容请求无可用 tier 或 spawn 失败时仍拒绝；永不回退为普通宿主进程。
- 兼容 tier 下若原请求允许网络，将网络收窄为仅允许公网出站连接，并拒绝私有网络入站和 host loopback。原请求拒绝网络时继续全拒绝。其它文件范围、工作目录、环境和进程生命周期边界保持原授权值。
- AppContainer+DACL tier 可在授权路径上临时调整 ACL，MXC 在进程结束时恢复。Shell 结果显示当前兼容 tier、ACL 临时调整及网络降级说明；日志记录 tier 和 probe warnings。
- 不新增设置项、配置字段、IPC 命令或数据迁移。

## 替代方案

- 只接受 BaseContainer：拒绝，会继续使一批具备 AppContainer 支持但没有新 BaseContainer 合同的 Windows 主机无法使用 WorkspaceWrite Shell。
- 退回当前用户权限的普通 host process：拒绝，会使工作区写入范围失效。
- 用 AppContainer 兼容 tier 保留原有入站/host-loopback 网络：拒绝，MXC 的兼容 tier 无法完整执行这项策略，因此将网络缩为仅允许公网出站。

## 影响与回滚

Shell 的 `WorkspaceWrite` 在兼容 tier 上可用范围扩大；兼容 tier 的文件隔离较 BaseContainer 弱，DACL 可能短暂变化，网络权限收窄为仅出站。调用结果会明确显示该降级。

回滚时恢复仅允许 BaseContainer 的检查，并删除兼容请求与提示字段；不改变 `FullAccess`、`ReadOnly` 或其他进程入口。

## 验证

- 单元测试覆盖兼容 tier 判定、策略拒绝、仅出站网络构造与拒绝网络保持拒绝。
- UI 测试覆盖 Shell 结果卡片显示降级说明。
- Windows 真实 MXC 验收需分别覆盖 BaseContainer 和 AppContainer+DACL 主机；当前开发机的 BaseContainer 路径继续由既有 Windows Shell/MXC 验收覆盖。若当前主机只提供 BaseContainer，则 AppContainer+DACL 真机路径记为环境受限，不据此宣称已运行验收。
