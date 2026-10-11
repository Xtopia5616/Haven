# ADR 0974：用 Base64 字节表达 MCP 二进制内容限额

## 状态

已采纳并实施。

## 背景

`mcp_max_binary_payload_bytes` 看起来表示解码后的媒体大小，但 MCP content normalizer 保留的是 JSON 字符串中的 Base64 表示，并以 `String::len()`（UTF-8 字节）决定是否保留该内容。配置注释和设置页虽然提到 Base64 字符/字符串，却与字段名产生了歧义。图片/音频与资源输出也没有统一说明这段长度：资源的 `bytes` 表示估算解码大小，图片/音频 oversized marker 的 `bytes` 实际是 Base64 长度。

## 决定

1. 配置、生成的 Settings contract、UI 与 MCP client 参数将限额命名为 `mcp_max_binary_base64_bytes` / `max_binary_base64_bytes`，明确限制保留在 observation 中的 Base64 UTF-8 字节数。门槛值和默认 2 MiB 不变，不改为解码后媒体大小。
2. 二进制输出中使用 `base64_bytes` 表示 Base64 UTF-8 字节长度；资源原有 `bytes` 字段继续表示估算的解码后大小。图片/音频 oversized marker 不再把 Base64 长度放进含义模糊的 `bytes` 字段。
3. 旧 TOML key 不添加 alias；需要保留配置时将 key 手动改为 `mcp_max_binary_base64_bytes`。数据库不变。

## 替代方案

- 将限额实现改成解码后字节数：拒绝；这会改变既有 observation 大小边界，而且实际被保留和序列化的是 Base64 字符串。当前字段应说明它真正约束的表示。
- 保留 `mcp_max_binary_payload_bytes` 并依赖注释：拒绝；字段本身仍无法区分 Base64 字符串长度与原始媒体大小。

## 影响与验证

- MCP 二进制保留/截断行为及默认值不变；输出元数据标明 Base64 表示长度，资源解码长度仍以 `bytes` 提供。
- 配置测试覆盖默认值、当前 TOML 解析和旧 key 拒绝；MCP 测试覆盖图像、音频与资源大小字段；Settings UI 保存测试覆盖设置回环。
- 不改数据库、MCP wire 请求或 transcript 格式。

## 回滚

恢复旧字段名并同步回退 MCP 参数、输出 metadata、Settings contract、测试、发布说明和 ADR。使用新 key 的配置需在回滚前改回 `mcp_max_binary_payload_bytes`；数据库无需处理。
