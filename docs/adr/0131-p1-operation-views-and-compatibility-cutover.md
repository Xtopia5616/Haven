# ADR 0131：P1 operation view、媒体页游标与内部兼容层切换

- 状态：已接受
- 日期：2026-09-12
- 范围：`haven-tools`、`haven-memory`、`haven-llm`、`haven-agent`、`haven-app-binary`、UI
- 关联：ADR 0127、0128、0129、0130；`docs/model-experience-optimization.md`

## 背景

P0 已经收敛能力快照、structured-first observation 和可恢复文件读取，但模型仍面对三个
明显摩擦点：`files`/`system` 的聚合 schema 暴露了无关 operation；文档抽取把多个逻辑页
压成一段文本；空 memory 结果只有 `no_hits`，无法区分来源和下一步动作。同时，测试版内部
仍残留 `Supplement`/`FollowUp`、confirmation 的 `trust_session`、ask sentinel、rollback
文本匹配和 provider 名推导 wire 协议等双入口。

## 决策

### 1. Operation view 与聚合实现分层

模型目录注册以下独立瘦视图：

- `files.read_text`
- `files.outline`
- `files.summary`
- `files.search`
- `system.info`

每个 view 固定 operation/scope，只发布该 operation 的参数 schema；执行、权限、取消、重试、
超时和 session 注册仍委托给聚合工具的同一个实现。聚合工具保留给 native/Tauri 调用和
低频或写操作。view 名称进入 session catalog，因此恢复按工具身份处理，不能通过意图猜测
隐藏 operation。

### 2. 搜索、outline 与文档抽取返回可继续消费的游标

`files.search` 返回 `root`、`pattern`、`has_more`，每个命中包含稳定的 `match_reason` 和
`context.before/after`。`files.outline` 返回 `range`、`symbol_count`、`has_more` 和
`next_page.start_line`。

`media(operation="extract")` 对受支持文档返回零基 `page_index`、`total_pages`、`next_page`
和 `has_more`。PDF content stream、DOCX/PPTX content part、XLSX worksheet 是受限抽取器的
逻辑页/section；聚合 native helper 仍可一次取全量文本。游标只由 extractor 生成，不从
模型可见正文猜测。

### 3. 视频按 provider capability 走 native wire

公共 `ContentPart` 增加 `Video`，`RawVideo` 通过 media projection 进入该 part。Gemini
capability profile 支持将其发送为原生 `inline_data`；OpenAI/Anthropic adapter 对未声明
支持的视频只返回显式文本占位，禁止静默转成图片或丢弃。当前切片覆盖 inline payload 的
provider wire；托管视频上传 API、视频转码和 keyframe 抽取依赖具体 provider 能力，仍需
另行评审，不在本 ADR 中伪装成通用能力。

### 4. 低风险偏好与 memory 诊断

`preferences` 和 `checklist` 是按 session 隔离的轻量内存工具，风险为 Low，不发起 ask/confirm
暂停，也不承担副作用授权。memory 空结果保持成功，只额外返回 keyword/vector 来源状态和
`broaden_query`、`try_other_kind`、`remove_subject_filter`、`configure_embeddings` 等
建议动作。

### 5. 测试版兼容层一次切换

- 内部输入类型和队列 API 只保留 `FollowUp`；`AgentEvent::Supplement` / `ProcessResult::Supplemented`
  作为已登记的跨端 wire 名称，不作为内部 facade。
- confirmation 请求只接受 `step_id`、`effect`、`scope`；删除 `confirmed`、`trust_session`
  bridge 和缺字段猜测。
- ask 恢复只接受显式 awaiting/typed result 以及 step/message 共享 id；删除 sentinel、
  observation 文本猜问题和内容去重。
- rollback 只接受精确 `msg-*` 身份；删除 content/prefix 匹配和 UI 的内容+时间反查。
- wire protocol 只由显式 `api_style` 选择；vendor/provider identity 不再隐式推导协议。

旧 snapshot、旧 confirmation 调用和旧数据库投影不做运行时兼容；发布边界按
`docs/release-and-reset.md` 备份/重置。外部 provider wire 差异、平台编码回退、故障恢复
和用户导航 redirect 不属于本次内部兼容层清理。

## 结果

模型能以独立 schema 直接选择常用读 operation，文档续读不会依赖正文切割，视频能力不会
跨 provider 静默降级，memory 空结果可行动；同时删除了会改变身份或授权语义的内部猜测
路径。仍保留的 snapshot-less projector、inbox 双消费、scheduled row fallback 和 prompt
旧布局属于后续 P2/灾难恢复边界，不能在本 ADR 中重新扩展。

## 验证

- Rust workspace fmt/check/clippy/test。
- UI check/test/build。
- document page cursor、search/outline metadata、memory diagnostics、preferences/checklist
  和 video ContentPart 单元测试。
- confirmation、ask resume、rollback identity 和 provider explicit-style 回归测试。
