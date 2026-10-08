# ADR 0752：Tool result renderer 只校验展示消费字段

## 状态

已采纳并实施。

## 背景

ADR 0745 将 builtin ToolResult 的运行时校验限定为专用 renderer 实际消费的字段。复核组件 props 与 registry guard 时发现两处实现偏宽：Clipboard 每行只展示 `content`，guard 还校验不展示的 `timestamp_ms`；Process renderer 的表格只读取进程行及 `operation` / `killed`，guard 还校验 `name_filter`、`count`、`matching_count`、`returned` 和 `limit` 等未由组件读取的列表元数据。

这些额外检查会因无关字段类型不符而将可正常渲染的 builtin 输出降级为通用 JSON，超出 presentation guard 的职责。

## 决定

- Clipboard entry guard 只要求 renderer 使用的 `content` 为字符串；额外 timestamp 字段保留在原 payload，不影响 renderer 选择。
- Process guard 只校验组件读取的 operation、kill PID 与进程数组/行字段；工具输出中未被该组件消费的列表元数据保持开放。
- 若未来 renderer 开始展示这些字段，再将其加入静态 props 和对应字段 guard。

## 影响与验证

只调整 UI builtin presentation guard，不改变 producer、动态 ToolResult JSON、provider wire 或持久化。新增用例确认 malformed 的未展示 Clipboard timestamp 和 Process 列表元数据不会触发 JSON fallback；renderer 消费字段的 malformed 形状仍由既有用例拒绝。

## 回滚

恢复额外字段校验即可。没有 IPC、数据或持久化迁移。
