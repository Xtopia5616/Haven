# ADR 0123：Agent 原生媒体工具契约

日期：2026-09-10
状态：已采纳

用量统计与模型观察收敛由 [ADR 0124：媒体推理用量与 Agent 缓存率边界](0124-media-usage-cache-boundary.md)
补充；本 ADR 的媒体入口和资产边界仍然有效。

关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0121：多模态表示持久化与快照边界](0121-media-persistence-and-snapshot-boundary.md)、
[ADR 0122：工具媒体请求统一入口](0122-unified-tool-media-entrypoints.md)

## 背景

上一阶段已经把工具侧的图片请求收敛到 `LlmRouter`，但模型仍需在
`files`、`window` 和 `audio` 之间猜测媒体入口：截图返回宿主路径，图片/音频/文档
能力又分别隐藏在文件读取分支中，工具结果不能自然地成为下一次工具调用的输入。

## 决定

1. 新增模型可见的 `media` 工具，唯一接受 `asset_id`，提供 `inspect`、`describe`、
   `transcribe`、`extract` 四个操作。它是受管图片、音频、PDF/DOCX/XLSX/PPTX 的
   统一派生入口。
2. `media` 只在工具边界解析受管路径并读取有界 bytes；图片请求统一调用
   `LlmRouter::analyze_image`，音频统一调用 `LlmRouter::transcribe_audio`，文档统一
   调用现有的有界本地抽取器。派生结果带 `MediaInput`、`MediaRepresentation`、
   provenance 和不可信内容标记，模型可以把同一个 `asset_id` 继续交给下一个操作。
3. `window.screenshot` 和 `window.ocr` 删除 `path` 参数。截图由宿主选择生成媒体目录和
   `file-{uuid32}.png` 文件名，登记为 `asset_id`，并使用当前会话 lease 与生成媒体清理
   生命周期；工具和 UI 都不再把宿主路径作为交互对象。
4. 对已经登记的图片/音频，`files.read` 转交到 `media` 的对应派生操作；`files` 的
   主职责收敛为文件系统读写、文本读取、搜索和文本摘要。
5. 媒体结果以结构化工具 observation 中的 compact `media` reference 传递，因此沿用
   现有 ReAct event authority 和 UI tool-card 投影，不维护第二条媒体 side channel。
   媒体内部 LLM usage 的持久化边界由 ADR 0124 定义。

## 安全与边界

- 模型输入只能是 `asset_id`；绝对路径、任意输出路径和 base64 不进入新工具 schema。
- registry 在每次派生前重新验证 managed root、普通文件和重解析点；图片/音频受现有
  bytes 上限约束，LLM 调用受现有 timeout 约束，文档继续受 `MAX_DOCUMENT_BYTES` 和
  文本预算约束。
- 图片描述、转写和文档提取都标记 `untrusted_content=true`；派生文本永远是数据，
  不能改变 Agent 的工具或安全策略。
- 窗口截图属于隐私敏感能力；OCR 继续为 High，普通 screenshot 由现有 window 授权
  边界控制。生成文件使用统一 expiry/lease 清理，不写入数据库 attachment 投影。

## 破坏性影响与重置

这是测试版本的模型工具契约破坏性收缩：`window.screenshot`/`window.ocr` 的 `path`
参数失效；managed binary 的 canonical 入口从 `files.read` 迁移为 `media`；新增
`media` 工具。没有数据库 schema 变更，但含有旧 window path 调用的未完成 snapshot/run
不保证恢复，遇到失败应重置数据根目录并重新开始会话。旧配置中没有新增迁移规则；如有
按工具名配置的 `media` 项，直接按当前 schema 重新配置。

## 文件边界

`builtin/media.rs` 负责 asset registry 边界、媒体操作分发和 canonical result projection；
provider 请求实现仍在 `haven-llm`，本地文档解析仍在 `tools/document.rs`，窗口平台捕获
仍在 `window.rs` 的 `imp` 模块。`window.rs` 仍保留较大的 Windows UI Automation/GDI
边界是因为它同时封装一组必须共享平台句柄和错误语义的原子操作；后续视频/缩略图能力
应扩展 `media` 的操作和 `haven-llm::media` 高层入口，不再把 provider 逻辑塞回 window/files。

## 验证与回滚

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools`
- `cargo test --locked -p haven-tools builtin::media -- --nocapture`
- `cargo test --workspace --locked`
- `cd ui; corepack pnpm run check; corepack pnpm run test:run`

回滚代码提交即可恢复旧实现；如果新版本已经产生包含新工具调用的 snapshot，回到旧二进制
前应按发布重置说明删除数据库与媒体缓存，不混用运行态数据。
