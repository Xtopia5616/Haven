# ADR 1295：按当前 envelope 投影 ToolRun 结果补充

状态：已接受并实施。

## 背景

Agent 的 ToolRun completion supplement 已使用带有 `<untrusted_tool_run_result>` 边界的 JSON envelope；UI 仍尝试解析早期文本格式，解析失败后会合成 `unknown` / `completed` 结果卡，可能把结果显示成错误身份和假成功。

## 决定

1. UI 只识别 Agent 当前 JSON envelope，并验证 canonical `toolrun-*`、`background`/`scheduled` kind、已知状态和字符串 summary。
2. 完整有效的 envelope 投影为 ToolRun 结果卡，内容保留 producer 的 JSON 数据并标记为 `result_injected`。
3. 缺字段、未知值或无效 JSON 不推断任务身份/状态；原 supplement 以普通文本保留。
4. 卡片幂等身份由 supplement ID 与 ToolRun ID 构成；无效文本 supplement 按其 supplement ID 去重。
5. Durable transcript 保持完整的不可信数据边界，不改变数据库或 ToolRun 持久格式。

## 验收

- Rust fixtures 使用当前 producer envelope。
- UI 校验当前 envelope、拒绝非法 payload、保留普通 supplement，并验证重复事件幂等。
- Rust workspace 与 UI type-check / tests 通过。
- 无持久化格式变化，无需数据重置。
