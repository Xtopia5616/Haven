# ADR 0129：资产优先的模型媒体入口统一

日期：2026-09-12
状态：已采纳

> 说明：ADR 0130 取代本 ADR 中关于 gateway/legacy adapter 的运行时实现与兼容承诺；当前版本以破坏性
> 的工具层统一为准。
> 独立 `audio` 模型工具随后由 ADR 0133 删除；本文的录音入口均指 `media(operation="record")`。

关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0121：多模态表示持久化与快照边界](0121-media-persistence-and-snapshot-boundary.md)、
[ADR 0122：工具媒体请求统一入口](0122-unified-tool-media-entrypoints.md)、
[ADR 0123：Agent 原生媒体工具契约](0123-agent-native-media-tool-contract.md)、
[ADR 0128：模型体验与实时工具契约优化](0128-model-experience-optimization.md)

## 背景

内部媒体骨架已经是 `MediaAsset → MediaRepresentation → MediaPlan`，但模型仍可能从
附件直投、`media`、`window` 和 `files` 四个相似入口获取信息：文件可能自己抽取，
OCR 可能绕过资产，录音可能只返回文本；能力 snapshot 与工具 schema 也可能只反映“配置了
某个模型槽位”，而不是当前路由真的能执行。

真正需要统一的是“获取信息”的入口，不是再发明一套媒体抽象。

## 决定

1. 模型侧采用两类入口：producer 只负责产生受管 `asset_id`，consumer 只通过
   `media.inspect/describe/ocr/transcribe/extract` 获取表示。每个媒体 observation 的
   `media` reference 固定包含 `asset_id`、`modality`、`available_representations` 和
   `recommended_next`，且不包含宿主路径、raw bytes 或运行时生命周期元数据。
2. `files` 不再承担媒体理解。`files.read` / `files.summary` 遇到图片、音频、PDF 或 Office
   路径时，先登记短期受管资产，再交给对应的 `media` operation；不支持或派生失败时仍返回
   同一个 `asset_id` 和 reference，模型可明确重试、转换或改走别的表示。普通文本、搜索、
   outline 和文件系统写操作保留在 `files`。
3. `window.screenshot` 只采集并返回资产；`window.ocr` 是薄的
   `screenshot → media.ocr` 便利封装。即使 vision 不可用，采集成功后也返回资产句柄，
   不把“没有 OCR”变成“没有截图”。
4. `media(operation="record")` 先保存 WAV 并登记资产，再尝试默认 STT；输出始终包含资产句柄，
   transcript 是可选的默认表示，失败时不丢弃录音。后续重新转写统一调用
   `media.transcribe`。
5. 工具装配根据实际路由角色、provider capability profile 和录音管线计算能力；schema
   裁剪与 runtime snapshot 共享这份判定。snapshot 使用 `vision=available|unavailable`、
   `stt=available|unavailable`、`tts=available|unavailable` 等语义状态，不把模型槽位配置
   当作能力真相。
6. 附件直接投影给 provider 仍由 `MediaPlan` 决定。对原始图/音，模型请求额外收到不含路径
   的短 `media_plan: asset_id → representation` notice，说明该表示已经在当前请求中，
   并提示后续派生使用 `media(asset_id=...)`；这不改变 provider 的 raw part 或持久化事件。

## 安全与兼容

- `asset_id` 仍由 host 通过 `haven_common::types::new_id("asset")` 生成；模型输入不能
  伪造路径、任意文件名或 base64。文件路径只在 trusted filesystem boundary 被 canonicalize、
  注册和 revalidate。
- 文件路径资产使用现有 managed-root / session lease / expiry 机制；录音、截图和附件不
  引入第二套清理器。派生文本沿用 provenance 与 `untrusted_content` 边界。
- `window.ocr`、`media.ocr` 的风险与确认门禁不因入口合并而降低；能力不可用只裁剪 schema
  或返回结构化不可用，不绕过授权。
- provider wire shape 不变；notice 只是 ReAct 请求副本中的文本 part，不写入 X12 事件、
  `messages` 或 `session_steps`。旧的 gateway 运行时不再由 legacy adapter 兼容；升级后的运行态按
  ADR 0130 的当前工具契约处理。

## 破坏性影响与重置

这是模型工具契约的破坏性收缩：rich `files` 读取从本地理解改为 `media` handoff，
`media(operation="record")` 的返回 shape 增加 `asset_id/media`，`window.ocr` 的无 vision 行为改为
先保留截图句柄。未完成的旧 tool call 可能期待 `files` 的旧文档正文；升级后应按当前 schema
重新执行该调用，若旧 snapshot 无法恢复则按发布重置说明清理会话和媒体缓存。数据库 schema、
实体 ID 格式和 X12 事件权威不变。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools`
- `cargo test --locked -p haven-tools builtin::files -- --nocapture`
- `cargo test --locked -p haven-tools builtin::media -- --nocapture`
- `cargo test --workspace --locked`
- `cd ui; corepack pnpm run check; corepack pnpm run test:run`

回滚代码与本 ADR 即可恢复旧入口；无需数据库回滚，但若新 snapshot 已包含新媒体 tool
observation，切换旧二进制前必须按发布说明重置不兼容的运行态数据。
