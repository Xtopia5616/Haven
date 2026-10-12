# 1018：校验录音事件 envelope 与 VAD 闭枚举

## 状态

已采纳并实施（2026-10-12）。

## 背景

Recording event mapper 是唯一未校验 Tauri event envelope ID 的 event owner。VAD producer 实际只输出固定 signal/state 集合，但 Rust DTO 将两者声明为 `String`，UI 对缺失或错误类型静默替换成 `none` / `silent`，因此 malformed payload 会伪装成正常状态，未知值也能进入 overlay。

## 决定

- recording event mapper 在读取 payload 前校验 event channel、非负安全整数 envelope ID 和 object payload；不合规事件返回 `null`。
- App wire DTO 用 `VadSignalDto` 与 `VadStateDto` 闭枚举序列化既有 snake_case 值；IPC generator 导出值集合，UI mapper 按生成集合校验，未知或畸形字段失败关闭。
- VAD 消费者只接收已验证的 signal/state；删除静默伪造默认值的路径。
- `check-ipc-events.ps1` 检查 envelope 和生成枚举 validator，防止 event contract 漂移。

## 影响

合法 event channel、序号格式与 JSON VAD 值保持；未知 VAD 值不再进入 renderer。event 顺序、节流和持久数据不变，无需重置。

## 替代方案

- 保留字符串并由 UI 复制手写列表：拒绝；会形成第二份枚举 authority。
- 畸形值回退到静默状态：拒绝；会隐藏 contract 错误并产生误导状态。

## 验证

- Rust `VadStatusEvent` serde test 验证闭枚举 wire 值。
- UI contract test 覆盖有效/未知 VAD 值、畸形 payload 与非法 Tauri event ID。
- IPC event checker、Rust workspace 门禁、UI tests/check 与 IPC 生成漂移检查通过。

## 回滚

恢复 `String` DTO 和 UI 默认值会重新允许未知值进入 overlay；不涉及数据迁移或 schema 重置。
