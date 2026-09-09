# ADR 0111：多模态附件与模型能力边界

## 背景

用户附件在前端以统一列表提交，但后端此前只把图片视为模型内联媒体：音频虽然会被媒体网关识别，
在后续落盘阶段却被当作普通文件，ReAct 只能把文件路径交给模型。另一方面，Anthropic 适配器对不支持
的音频输入会静默丢弃内容，导致“模型已收到请求”与“模型实际看到的内容”不一致。浏览器提供的 MIME
也不是可靠的契约，SVG、AAC 等仅靠扩展名才能识别的格式容易退化成
`application/octet-stream`。

## 决定

1. `MessageAttachment` 统一区分两类输入：图片和音频是 `is_inline_media()`，保留 base64 并转换为
   provider-neutral 的 `ContentPart::Image` / `ContentPart::Audio`；其他文件落盘并只以路径文本交给
   `files` 能力。图片与音频仍共用同一个附件列表和持久化消息契约。
2. MIME 归一化在 Tauri host 边界完成：先看 magic bytes，再以文件名扩展名兜底。媒体网关和生成文件
   附件复用同一套 MIME 推断函数，禁止各层自行猜测。
3. ReAct 在请求快照上一次性计算媒体需求：图片走 `vision_role`，只有音频走 `audio_role`，无媒体走
   `default_model`。混合图片/音频请求保留图片优先级，避免同时把一个请求拆成两个模型调用。
4. provider adapter 必须在发送前校验无法表达的内容。Anthropic 对音频返回
   `UnsupportedCapability`，不得静默删除音频块；调用方可以配置 Audio Model 或专用 STT 作为降级路径。
5. 本次不扩展视频的 provider wire 契约。视频继续按普通文件处理，直到新增明确的 provider capability 与
   生命周期设计；不能因为 `Modality::Video` 已能检测就把不兼容的二进制盲发给聊天接口。

## 替代方案

- 在 agent 层把所有非图片都变成文件路径：拒绝，音频理解会退化成无法消费的路径提示。
- 在 Anthropic adapter 中直接过滤音频：拒绝，属于静默数据丢失。
- 为每种 provider 在前端维护一套附件分类：拒绝，provider wire 能力属于 `haven-llm`，前端只负责采集和展示。
- 立即引入视频内联：拒绝，当前各 wire adapter 的视频支持不一致，需另立 capability / 文件上传设计。

## 影响与验证

- 不改变数据库 schema 或已有图片/普通文件字段；新行为只扩展音频附件的运行时投影。
- 音频附件受现有文件数量和文件大小上限约束，但不再写入上传目录；消息恢复时仍可使用其 base64 数据。
- 必须通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、
  `cargo clippy --workspace --locked -- -D warnings`、`cd ui; corepack pnpm run check; corepack pnpm run test:run; corepack pnpm run build`。

## 回滚

回退本 ADR 对应提交即可恢复“只有图片内联”的旧行为；不需要数据库重置，已保存的音频附件仍可按普通
附件路径或消息数据读取。
