# ADR 0571：统一 Rust 当前状态 accessor 命名

## 状态

已采纳并实施。

## 背景

`InputPipeline::get_state()` 返回录音生命周期状态，`DesktopShell::get_state()` 返回 shell 状态快照。二者都读取接收者自身状态，没有查询 key；Input 的附加 VAD 状态读取又叫 `get_vad_state()`，而 `VadDetector` 已使用 `state()`。全仓方法命名因而把“按 key 取值”和“读取 owner 自身状态”混在一起。

## 决定

1. Rust owner 的主状态读取使用 `state()`；InputPipeline 暴露的 VAD 子状态使用 `vad_state()`。
2. 同步更新 App、Tools 和 Input 内所有调用点及测试。
3. 保留 Tauri 命令 `get_recording_state`，因为它是 IPC 命令名；本 ADR 不改变 command、payload 或 UI wrapper。
4. 命名规范明确 `get_*` 用于按 key 读取，接收者状态使用名词式 accessor。

## 替代方案

- 保留通用 `get_state`：拒绝，会继续把 owner 自身快照读取伪装为按 key 查询。
- 所有状态方法都加 owner 前缀（如 `recording_state`、`shell_state`）：拒绝，Rust 的 receiver 类型已限定 owner；同一 owner 的多个状态视图才需要领域限定词。
- 改为 `snapshot()`：拒绝，`DesktopShell` 返回副本，但 `InputPipeline::state()` 表示当前状态且不应暗示复制/版本化快照语义。

## 影响与验证

- 仅重命名 workspace 内 Rust 方法及调用点；状态读取、锁、克隆、IPC 名称和运行行为不变。
- 无数据库、配置、持久 ID 或 wire 契约变化，也无需数据重置。
- 验证：Rust fmt、workspace locked check、严格 Clippy 与串行 workspace tests。

## 回滚

将 `state()` / `vad_state()` 与各调用点恢复为 `get_state()` / `get_vad_state()`，并撤回命名规范的 accessor 规则。
