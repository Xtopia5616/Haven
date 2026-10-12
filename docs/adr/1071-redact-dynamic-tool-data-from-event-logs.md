# ADR 1071：从事件日志中移除动态工具数据

## 状态

已接受（2026-10-13）

## 背景

Agent event bridge 将动态 tool name 写入普通日志。工具名可能来自模型或用户配置；与此同时，若干长度字段没有说明计量单位，Thought/参数 UTF-8 内容与字符数容易混淆。确认日志也重复记录了完整的动态工具名。

## 决定

- ToolCall、Observation、ToolCallChunk 和确认处理日志不记录动态 tool name；不记录工具参数或观察正文。
- 仍需诊断文本规模时，UTF-8 字节数使用 `*_bytes`，Unicode 字符数使用 `*_chars`。
- 其它低敏稳定身份、序号及状态字段保持现有日志级别和内容。

## 影响

普通日志减少动态名称暴露，长度诊断单位明确；UI event 内容和日志等级不变。无持久数据、配置或 IPC 迁移。

## 验证

- App event bridge 捕获日志测试验证私有 tool name/参数不出现，并验证多字节中文/重音字符的 bytes/chars 计数。
- 运行受影响 App/Agent crate 的测试、check 与严格 Clippy。
