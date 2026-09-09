# ADR 0110：模型工具驱动的 TTS 与通知边界

## 背景

Haven 已有 TTS provider 配置和客户端，但原先 `audio` 工具只能录音、播放 WAV、控制音量，
而媒体网关还会根据用户文本中的朗读类关键词尝试自动生成语音。这样会让“回答内容”和“系统
提醒”混在一起，也让用户无法判断一次声音输出来自模型决策还是输入预处理。`notify` 则已经
承担应用内 toast 与 Windows 桌面通知的提醒职责。

## 决定

1. 在现有 `audio` 工具中增加 `operation="speak"`，由模型在确实需要用户听到内容时显式调用；
   文本限制为 4,000 个字符，返回播放结果，不创建消息附件。
2. TTS client 在应用组合根构造后注入 `haven-tools`，设置热更新时与 router 一起刷新。未配置或
   构造失败时，`audio.speak` 明确报错，其他音频能力继续可用。
3. 本机播放只接受 WAV：OpenAI-compatible provider 请求 WAV，ElevenLabs 的 PCM 结果在应用内
   包装为 WAV，再由 Windows WinMM 同步播放。同步播放保证 provider 缓冲区在播放结束前有效；
   合成阶段支持取消，当前 WAV 开始播放后完成这一段再返回。
4. `notify` 只负责提醒和状态反馈，保持应用内 toast + Windows 桌面通知双通道，不触发 TTS。
   `audio.speak` 只负责扬声器内容，不触发通知。两者都需要时由模型分别调用；Windows 自己
   播放的系统提示音不视为 TTS。
5. 媒体网关不再自动识别朗读/配音关键词，也不再在 ingress 阶段调用 TTS；文生图仍由媒体网关
   处理。此前 ADR 0089 中“未来另行设计独立 speak 工具”的表述由本 ADR 对主动播报部分取代，
   但本实现将 `speak` 放在 `audio` 工具内以保持音频能力单一入口。

## 替代方案

- 保留 ingress 关键词自动朗读：拒绝，会产生隐式副作用，且无法与普通文本回答的生命周期区分。
- 新增顶层 `speak` 工具：拒绝，用户要求音频内置工具支持 TTS，且录音/播放/播报共用音频安全
  与 UI renderer 边界。
- 让 `notify` 自动附带 TTS：拒绝，通知是注意力通道，不能假定每条提醒都应打断扬声器内容。
- 直接播放 MP3 或启动外部播放器：拒绝，增加解码器/进程边界；当前 WAV/PCM 方案足以覆盖
  已支持的 provider。

## 影响与验证

- 模型工具目录新增 `audio.speak` schema，风险为 Low；`speak.text` 会发送给配置的 TTS
  provider，长度有上限，工具结果不回显文本内容。
- 旧配置无需迁移；`media.tts` 仍是 provider/model/voice 的配置来源。数据库 schema 不变。
- 必须通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、
  `cargo clippy --workspace --locked -- -D warnings`、`cd ui; corepack pnpm run check; corepack pnpm run test:run; corepack pnpm run build`。

## 回滚

回退本 ADR 对应提交即可移除 `audio.speak` 与模型工具注入；回滚前不需要数据库重置，旧配置
中的 `media.tts` 字段可继续保留。
