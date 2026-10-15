# ADR 1208：在 UI 中审阅并逐项晋升 sandbox 输出

## 状态

Accepted

## 背景

ADR 1200–1207 已建立 AppContainer 输出快照、短期 review owner、按 manifest index 晋升新文件以及 Tauri Once-only 授权续接。用户仍缺少读取变更清单、显式丢弃临时输出和选择晋升目标的产品入口。

把完整工具结果或绝对 sandbox 路径放进 Shell observation 会同时扩大模型上下文和 renderer 的文件系统权限；由 renderer 自行声明源路径也会绕过 owner 对不可变 manifest 的绑定。

## 决策

1. App 只通过 `SandboxOutputReviewPort` 查询、丢弃 Tools 持有的 review；查询参数必须是规范 `ses-*` 与 `step-*`，且对应持久步骤属于同一会话的 `shell` 工具。
2. IPC 使用 App-owned DTO，只暴露相对显示路径、变更类型、字节数、摘要、索引、过期时间和晋升状态；不暴露文件内容、临时目录绝对路径、目录 capability 或 execution proof。
3. Shell 结果卡片按当前会话和 `step-*` 查询 review。UI 仅可对 `Created` 普通文件发起晋升；Modified、Deleted、TypeChanged 只显示摘要。
4. 用户通过原生保存对话框选择目标候选。选择本身不授予访问；目标继续由当前 Files workspace policy 验证，并进入 ADR 1207 的必需 Once/Operation 确认和 proof-bound 晋升。
5. review owner 在创建目标成功后标记对应 manifest index 已晋升，同一变化项不可重复晋升。用户可单独丢弃整个短期 review；原有容量上限和 15 分钟 TTL 不变。
6. 授权请求排队后 UI 提示用户处理全局确认窗口，并提供显式刷新；拒绝或尚未完成时不会把文件标成已保存。

## 备选方案

- 将 manifest 注入模型可见的 Shell 结果：会把 UI 控制信息和路径元数据加入模型上下文，并让用户审阅失去独立入口，因此不采用。
- 由 renderer 保存 sandbox 源路径并直接复制：会绕过 owner 持有的 workspace、摘要和 policy stamp，因此不采用。
- 一次性复制整个 workspace：无法约束副作用范围、结果大小或用户选择，因此不采用。

## 影响

- 新增 Tauri read、mutate、native picker 命令与 App-owned camelCase manifest projection；无持久化 schema 或配置变更。
- review 内容仍是进程内短期数据，重启后由 RAII 清理；TTL 和手动 discard 释放存储配额。
- UI 和 review IPC 已接入，但在前台 Shell 尚未改走 production AppContainer launcher 前，不会有普通产品运行生成的 review。该执行接线单独作为后续切片。

## 验证

- Rust owner 测试覆盖成功晋升状态和重复晋升拒绝；App 测试覆盖 canonical ID、session/step Shell 身份与 DTO camelCase 投影。
- UI 测试覆盖安全 manifest 校验、仅 Created 文件可选、一次性确认排队、刷新状态和显式丢弃。
- Windows AppContainer 端到端 Shell 生产验收留在后续 launcher 切片。

## 回滚

无持久化格式变化。移除 UI、IPC 和 owner 晋升状态即可回滚；运行中的 review 在进程退出时由现有 RAII 生命周期清理。
