# ADR 0113：统一多模态资产、表示与请求投影

日期：2026-09-10  
状态：分阶段实施（阶段 0 已采纳）

## 背景与当前契约盘点

Haven 的前端用一个附件列表提交图片、音频和普通文件，但后端目前仍把
“附件元数据、原始 bytes、派生文本、落盘路径、provider content part”混在
几个不同边界里：

| 当前入口 | 当前权威实现 | 现状与风险 |
|---|---|---|
| 浏览器附件 | `ui/src/lib/InputRouter.svelte`、`process_transcript` | 附件以 base64 进入 Tauri；前端限制不是安全边界 |
| host 校验/落盘 | `crates/app-binary/src/commands/recording.rs` | 普通文件写入 `uploads/<batch>` 并清空 `data`；图片/音频继续把 base64 带入消息 |
| 消息持久化 | `crates/memory/src/repositories/messages.rs` | `MessageAttachment` JSON 直接写入 `messages.attachments` |
| 媒体派生 | `crates/llm/src/media/gateway.rs` | OCR/STT 成功后返回文本，但调用方仍保留原始附件 |
| ReAct 投影 | `crates/agent/src/react/mod.rs`、`types.rs`、`resume.rs` | 依据附件类型直接构造 `ContentPart`；普通文件可能把绝对路径写入模型文本 |
| provider 路由 | `crates/agent/src/react/turn.rs`、`crates/llm/src/router.rs`、`adapters/` | 根据已有 `ContentPart::Image/Audio` 选角色；没有统一的能力画像和降级理由 |
| 配置热应用 | `crates/common/src/config/service.rs`、`app-binary/src/config_runtime.rs` | `MediaConfig` 已支持热更新，但尚无输入表示策略 |

已存在的 ADR 0111 继续约束当前兼容行为：图片/音频是现有 inline media，
普通文件走受管路径，视频暂不增加 provider wire part。ADR 0113 扩展其内部
契约，不立即替换所有入口。

## 决定

### 1. 统一内部抽象

后续代码以三层概念表达一次输入：

1. `MediaAsset`：一个受管资产或外部引用。包含 `asset_id`、内容 hash、MIME、
   大小、展示文件名、来源和生命周期；不包含需要写入 transcript/snapshot 的
   大段原始 bytes，也不向模型暴露绝对本机路径。
2. `MediaRepresentation`：同一资产的一个可消费表示。表示种类包括
   `raw_image`、`raw_audio`、`raw_video`、`extracted_text`、`transcript`、
   `ocr_text`、`image_description`、`document_pages`、`table_data`、
   `thumbnail` 和 `managed_file_ref`。每个表示都带原始/派生 provenance、
   可选 confidence/cost 和 availability。
3. `MediaPlan` / `RequestProjection`：在一次 provider 调用边界，根据用户策略、
   表示可用性和目标模型能力选择要发送的表示。provider adapter 只序列化已经
   选择的最终投影，不再重新猜附件类型。

阶段 1 的兼容适配器可以从旧 `MessageAttachment` 构造临时 `MediaAsset` 和
表示；阶段 2 以后再把 live ingress、ReAct、gateway 和 resume 逐步切换到该
计划。旧字段读取必须继续工作，直到对应迁移阶段明确删除。

### 2. 原始与派生输入严格分离

- 原始表示代表用户提供的 bytes 或用户明确引用的原始文件。
- 派生表示代表 OCR、STT、文档抽取、图像描述等处理结果。
- 派生文本在消息/模型上下文中必须保留 provenance，并使用与用户显式文字
  可区分的内部标记；不能把它伪装成用户原话。
- 派生成功不会自动授权继续发送原始媒体。若目标 provider 不支持该原始
  modality，计划器必须选择派生表示、受管引用或安全失败，不能把不兼容的
  原始 part 留在同一个请求中。

### 3. 能力画像使用三态，不按模型名猜测

`CapabilityProfile` 至少描述 `text`、`image`、`audio`、`video`、
`native_file_upload`、`tools`，以及允许的 MIME、输入数量、单项/总大小和
上下文限制。每项能力为 `supported`、`unsupported` 或 `unknown`。

计划器的保守规则：

- 只有 `supported` 才发送原始媒体或 native file upload；`unknown` 不视为
  支持。
- 文本派生也必须满足文本能力；默认 provider profile 应显式填入 text，
  不依赖“文本总是可用”的隐式假设。
- 受管文件引用需要 `tools` 或 `native_file_upload` 的明确支持；引用只携带
  `asset_id`/受控展示名，不携带绝对路径。

### 4. 输入表示策略

策略名称固定为：`auto`（默认）、`raw_preferred`、`extracted_preferred`、
`text_only_safe`。

| 策略 | 选择顺序 | 禁止行为 |
|---|---|---|
| `auto` | 支持的 raw → 安全派生文本 → 受管引用 | 不把 unknown 当 supported |
| `raw_preferred` | 支持的 raw → 派生/引用，并产生降级原因 | 不能因偏好 raw 盲发不兼容媒体 |
| `extracted_preferred` | 派生文本 → 支持的 raw → 受管引用 | 派生失败时不得伪造文本 |
| `text_only_safe` | 用户文本 + 安全派生文本 | 不发送 raw、thumbnail 或仅路径引用 |

默认 `auto` 保持当前已支持的图片/音频能力；策略接入 `MediaConfig` 和设置
页面属于阶段 4，保存后走已有 `ConfigService`/`ConfigChanged` 热应用链路。

### 5. 持久化与生命周期边界

阶段 0–2 不新增 SQLite 表，也不把 base64 大对象迁移到 snapshot：

- `ReActSnapshot.events` 仍是事件权威；事件只保存受限的资产元数据、引用和
  表示 provenance，禁止保存大段 bytes。
- `messages.attachments` 继续读取旧 `MessageAttachment`，作为兼容投影；新
  写路径在迁移完成前只允许存小型 inline payload，普通文件存受管引用/元数据。
- `session_steps` 不复制资产内容，只保存执行态和关联 id。
- 受管文件有明确的数量、大小、TTL、取消清理和进程重启清理策略；任何需要
  新数据库表或 schema 版本的变更必须另立迁移/重置 ADR，遵守当前“不同版本
  直接重置、不做运行时迁移”的规则。
- 历史消息可继续展示旧 base64/path；恢复时先读取 legacy attachment，再由
  兼容适配器生成计划，不用内容比对去重。

### 6. 安全与 prompt-injection 边界

- 文件名只作为受限展示标签，必须去除控制字符、截断并经过现有安全文件名
  处理；路径永不作为 provider-facing 业务内容。
- 模型看到的文件引用使用不可猜测的 `asset_id` 和安全标签；需要访问本机
  文件时由 `files` 工具在受管边界解析。
- OCR/STT/文档抽取结果是“不可信外部内容”，在 prompt 中用明确 provenance
  fence 包裹，不得提升为系统/用户指令；后续提示词渲染要复用已有字段清洗与
  长度上限。
- 日志只记录 asset_id、模态、大小、计划结果和脱敏原因，不记录 base64、完整
  路径、文件正文或 provider secret。

## 分阶段迁移与回滚

| 阶段 | 范围 | 退出条件 | 回滚边界 |
|---|---|---|---|
| 0 | 契约盘点、ADR、错误/持久化/测试矩阵 | 文档与现状一致，默认行为不变 | 仅回滚文档提交 |
| 1 | `haven-common` 内部类型、能力画像、纯计划器和单测；旧附件适配器不改行为 | 计划器覆盖 raw/derived/ref、三态能力、四策略、限制和安全失败 | 回滚类型/测试提交，不需重置 DB |
| 2 | ingress、ReAct、gateway、provider projection；修复派生成功仍发送不兼容 raw 的问题 | mixed media、历史恢复、取消/失败降级、无绝对路径 provider 输入有回归测试 | 回退运行时投影提交；保留旧消息读取 |
| 3 | files/audio/window 等工具接入 asset/representation，受管文件 TTL/清理 | 工具只通过 asset_id/representation 交互，生命周期压力测试通过 | 先回退工具接入，再处理受管文件目录 |
| 4 | MediaConfig 策略、热应用、设置 UI、降级原因展示 | 四策略端到端验证，默认 auto 与旧配置等价 | 配置字段若需删除，按发布/重置说明处理 |
| 5 | PDF/Office/表格/图像增强；视频/keyframe 另行 capability 评审 | 每种表示有 provider 矩阵和资源上限 | 视频不纳入此前阶段的回滚假设 |

## 降级矩阵

| 原始表示 | provider capability | 派生可用 | 计划结果 |
|---|---|---|---|
| raw image/audio/video | supported 且 MIME/大小通过 | 任意 | raw |
| raw media | unsupported | 有安全文本派生 | derived text，记录 fallback |
| raw media | unknown | 有安全文本派生 | derived text，记录 capability unknown |
| raw media | unsupported/unknown | 无派生，受管引用可用 | managed ref（仅工具/文件上传支持时） |
| raw media | unsupported/unknown | 无安全表示 | 明确失败或用户可见降级提示，不静默丢失 |
| 任意 raw | `text_only_safe` | 有安全文本派生 | derived text |
| 任意 raw | 任意 | 派生只返回空/错误 | 保留原始输入供后续重试，但本次请求不发送不兼容 raw |

## 测试矩阵

- 纯单元：资产 id/hash/元数据、表示 provenance、三态 capability、MIME/数量/
  大小/上下文限制、四策略选择、mixed image+audio、空派生和 unknown 能力。
- ingress/gateway：OCR/STT 成功时 raw 与 derived 的互斥投影；失败时不伪造
  派生；生成附件和普通文件的生命周期。
- ReAct：live apply 与 `project_transcript` 一致、request retry 不污染 durable
  events、snapshot/resume 只保留元数据、不因内容相同去重。
- provider：每个 adapter 对不能表达的 projection 返回 `UnsupportedCapability`，
  不静默过滤；最终 wire payload 不含绝对本机路径或 base64 日志。
- 工具/安全：文件名 traversal、UNC/reparse、过期 asset、数量/大小上限、取消
  清理、prompt-injection fence 和脱敏日志。
- 门禁：适用阶段运行 Rust fmt/check/clippy/test；跨端阶段再运行 UI check、
  test:run 和 build。

## 暂不决定

- 不在本 ADR 中新增视频 `ContentPart`、keyframe 抽取或 provider 上传协议。
- 不把图片编辑/生成能力与输入理解共享同一套执行语义；生成仍由既有 gateway
  和显式工具路径负责。
- 不在没有真实 provider capability 来源前按模型名维护硬编码能力表；未知能力
  必须走保守降级。

## 回滚与重置

阶段 0–2 不改变数据库 schema，回退相应提交即可恢复旧运行时路径，已存消息仍
按 legacy attachment 读取。阶段 3 以后如引入 schema、配置字段或受管文件目录
语义，必须在对应阶段补充发布/重置说明；禁止混用不同版本的数据库、快照和
配置文件。
