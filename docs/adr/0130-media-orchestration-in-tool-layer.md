# ADR 0130：媒体编排统一下沉到工具层

日期：2026-09-12
状态：已采纳

> 补充：本 ADR 关于独立 `audio` 公共入口的决定已由 ADR 0133 supersede；当前仍保留其
> “设备实现与媒体编排分离”的边界，但模型只看到 `media`。

关联：[ADR 0129：资产优先的模型媒体入口统一](0129-asset-first-model-media-entrypoints.md)、
[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)

## 背景

`MediaGateway` 位于 `haven-llm`，会在用户消息进入 ReAct 前根据关键词自动做 OCR、语音转写或
文生图；新的 `media` 工具又在 `haven-tools` 提供资产操作。两条路径分别处理能力判断、fallback、
文件生命周期和用量，导致同一媒体能力存在两套入口，工具 schema、权限和历史也无法完整覆盖。

## 决定

1. 删除 `haven-llm::media::MediaGateway` 及其 ingress 预处理路径；`haven-agent` 只持久化用户原始
   文本和已登记的附件，不再根据自然语言关键词隐式调用媒体模型。
2. `haven-tools::builtin::media::MediaTool` 是唯一的媒体理解/生成编排边界，统一承接资产解析、
   OCR、STT fallback、文档抽取和显式 `generate`。每次调用都经过同一套工具 schema、权限、取消、
   超时、受管资产和 tool LLM usage 链路。
3. `files` 和 `window` 仍可作为 producer/便利入口，但共享同一个已装配的 `Arc<MediaTool>`；它们
   不再各自实例化媒体处理器。音频设备的录音、播放、TTS、音量和静音仍由独立宿主运行时承载，
   但公共模型入口由 ADR 0133 统一为 `media`。
4. 文生图必须由模型显式调用 `media(operation="generate", prompt=...)`；生成文件先登记为受管
   `asset_id`，再作为工具结果返回，不再被伪装成用户消息附件。

## 破坏性影响

- 删除 `MediaGateway`、`coverage`、`intent` 及其公开 re-export；旧的 gateway 测试和旧入口不保留。
- 删除 ingress 的自动 OCR/STT/文生图行为；旧 snapshot 或未完成 tool call 不做兼容转换，升级后按
  当前 schema 重新执行，必要时重置运行态会话。
- `media` schema 新增显式 `generate`，并要求非生成操作使用 `asset_id`；生成操作只接受 `prompt`。

## 安全与验证

原始附件仍由 host canonicalize、登记、revalidate 和 session lease 管理；模型不能提交路径、bytes
或 base64。OCR/STT 派生文本继续标记为不可信内容，媒体生成和 OCR 的风险等级进入同一安全矩阵。

验证至少包括 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`，以及前端
`corepack pnpm run check`、`corepack pnpm run test:run`。
