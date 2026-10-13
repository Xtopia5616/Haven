# 1141：按稳定领域身份渲染工具结果列表

## 状态

已采纳并实施。

## 背景

ToolResult renderer 的数组行曾混用内容、可空字段与索引作为 Svelte key。列表重排时这会把局部状态留在错误行；MCP tools/list 同名项也能通过边界并造成重复 key。ADR 1091 记录的 UI Automation token 可选及索引 fallback 已不符合当前 producer 与 guard。

## 决定

- 工具列表必须使用 producer 提供且在该列表唯一的领域身份：窗口使用 `window_id`，UI Automation 元素使用 `element_token`，Memory facts/hits 使用 canonical `fact_id` / `entity_id`，Clipboard history 使用 `entry_id`。
- Clipboard `entry_id` 使用 `clip-*` 进程内身份；内容重复时把既有 row 移到前面并保留 ID。它只服务 renderer，不是持久实体。
- Admin skill、MCP tool、MCP server 和环境变量列表按名称识别；相应 producer/边界拒绝重复名称或规范化去重，renderer 不用索引补身份。
- Ask options 在 producer、恢复 mapper 和 renderer 边界 trim、过滤空项并去重，避免相同标签形成重复 selection/key。
- WebSearch 没有 provider ID；renderer 用 `(url, title, snippet)` 和相同 tuple 的 occurrence 建立列表位置身份，不将展示内容称作实体 ID。
- UI Automation 的 `element_token` 必须非空且唯一，renderer 以 token 为 key；缺少 token 的 row 回退通用 JSON renderer。此决定仅 supersede ADR 1091 中 UI Automation 的 optional-token/index fallback；该 ADR 关于 Memory row 的决定继续有效。
- MCP `tools/list` producer 与 UI snapshot parser 都拒绝同一 server 内重复或空白 tool name。

## 影响

Renderer 的 keyed rows 在异步插入和排序变化时仍关联原 row；不合规或身份歧义的 payload 不进入专用 renderer。Clipboard ID 不写入数据库，不需要持久化迁移或重置。

## 验证

新增 producer/parser 的唯一性回归及 Clipboard、Memory、Window、WebSearch DOM reorder 测试；运行 Rust workspace 与 UI 门禁、MCP contract 生成漂移检查。

## 回滚

无数据重置要求。若修改 renderer identity，应同时调整 producer、guard 和 reorder/duplicate regression；不得恢复内容 key 或索引 fallback。
