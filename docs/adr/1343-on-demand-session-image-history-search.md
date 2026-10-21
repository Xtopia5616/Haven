# ADR 1343：按需检索会话关联图片

## 背景

压缩会话时，较早的图像可能不会出现在后续模型请求中，但用户输入、媒体生成和工具结果仍会在当前 transcript lineage 中关联图片资产。原始历史工具能够找回文本事件，却没有直接列出图片及其关联语境的操作；将所有旧图片重新注入 prompt 会抵消压缩收益。

## 决定

- 在 `session` 工具增加只读、幂等的 `image_search`，通过 Tools 注入的当前 session ID 搜索当前分支中的用户附件和工具产图记录。可用短文本查询关联用户语句、文件名、媒体类型及已有图片描述/OCR；不带 query 时列出最近图片。
- 每条结果按 transcript 事件分组，返回 sequence、关联语境、图片 `asset_id`、文件名和媒体类型；如历史中已有图片描述或 OCR，则按需返回有界片段。结果不包含原始图片字节、宿主路径或所有图片的视觉分析。
- 搜索跳过被 timeline rollback 移除的事件，并复用 transcript history 搜索的 50,000 event / 64 MiB scan ceiling；单次最多返回 5 组，每组最多包含 8 张图片，总引用数最多 10,000。超过任一引用上限时显式失败。
- `session.image_search` 只定位候选并提供上下文关系。模型需要视觉细节时，再使用既有 `media.describe` 或 `media.ocr` 和返回的 `asset_id`；图片不可用时由既有媒体边界显式失败。
- 关联语句与缓存图像文本是低信任数据。提示词和工具结果要求把它们视为数据而非指令。

## 替代方案

- 每轮把全部旧图片重新编码到 prompt：会引入额外媒体 token 和重复视觉分析，拒绝。
- 仅依赖 `session.history_search`：可以找到用户提到图片的文本，但模型仍需猜出资产身份或展开原始记录，拒绝。
- 复制一套媒体读取能力到 `session`：会绕开现有 `media` 工具的资产授权和内容处理 owner，拒绝。

## 影响

Memory 增加基于 append-only transcript 的有界关联图片检索 DTO/API，Tools 通过 `session.image_search` 暴露。无数据库 schema、配置或持久数据格式变化，不需要重置用户数据。工具只返回不透明 asset ID；媒体读取继续经过当前会话的受管资产验证。

## 验证

- Memory 测试覆盖用户附件关联、工具生成图像、缓存描述搜索和回滚分支过滤，并确认响应不包含字节或路径。
- Tools 测试覆盖 session ID 注入、检索结果及 catalog 延迟加载；Agent 提示测试确保模型知道按需查询路径。
- 跨 crate 检查需运行 workspace `cargo check`、严格 Clippy 和全量 `cargo test`，另检查 Rust 格式。

## 回滚 / 重置

回滚代码提交即可移除检索入口；已有 transcript 事件和媒体资产不变，无需迁移或重置数据。
