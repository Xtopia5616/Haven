# ADR 1161：分离 Provider 预设身份与线协议，并声明媒体端点能力

## 状态

已采纳

## 背景

设置页此前把预设 ID 与 `api_style` 放在同一表单字段。切换到 DeepSeek、Azure 等预设时，表单只留下线协议值；保存时再根据该值写回 provider identity 与认证头，可能将具体厂商折叠成 OpenAI，导致 Responses/OpenAI-compatible adapter 的厂商特调失去可靠身份来源。

媒体调用也只按 wire style 选择后端。OpenAI-compatible 聊天并不保证服务实现 `/audio/transcriptions`、`/audio/speech` 或 `/images/generations`，但旧规则会将 STT、TTS 和生图都暴露给此类 Provider。

## 决定

1. Provider 编辑表单独立保存所选预设 ID 与 canonical `api_style`。保存时，`provider` identity 和认证头从预设取得，wire protocol 使用表单中的 `api_style`。厂商扩展仍由 adapter 根据 `provider`、Base URL 与 model 检测，不新增 vendor wire style，也不移除 Responses 特调。
2. Provider 预设目录按服务属性分组；新增自定义 OpenAI-compatible、LM Studio 与 vLLM 预设。三个新增预设使用 `openai-chat`，不因 OpenAI-compatible 而自动启用媒体端点或 Responses 内置搜索。
3. `ProviderConfig.media_capabilities` 是可选的三个布尔值（`stt`、`tts`、`image_gen`）。显式值控制媒体 builder；缺省值用于读取旧配置时按已知官方服务 profile 推断。OpenAI 官方 endpoint 默认三项可用，Gemini 默认 STT/生图，Groq 默认 STT，Deepgram/AssemblyAI 默认 STT，ElevenLabs 默认 TTS；其它 OpenAI-compatible 与本地服务默认关闭。Settings 允许用户按 wire style 可实现的范围手动开启兼容 endpoint。
4. Common 校验显式启用的媒体能力与 wire style 支持面；LLM builder 在创建媒体 client 前再次校验 effective capability。Keyless 能力继续由 Provider profile 声明，LM Studio 被标记为 keyless，vLLM 保持需要凭据或由用户提供服务端 key。
5. `apiStyleCapabilities.ts` 负责 wire-style / media capability 逻辑；`providerPresets.ts` 负责预设目录、预设应用、identity 显示和 keyless profile；`apiStyle.ts` 只作为兼容 re-export 入口。

## 影响

- `config.toml` 的旧 Provider 缺少 `media_capabilities` 时仍可解析，字段序列化时省略 `None`；设置页保存后才写入显式值。无数据库 schema 变化，不需要数据库或配置重置。
- 非 OpenAI 官方通用网关的旧配置将不再默认暴露 STT/TTS/生图。需要这些路由的用户应在 Provider 编辑页确认服务端 endpoint 存在后启用对应能力。
- Responses/OpenAI-compatible vendor tuning 的识别条件与 adapter 实现保持原样；修复预设身份写回后，DeepSeek、Kimi 等 identity 不再被预设编辑覆盖成 `openai`。
- 新增 UI 预设不声称服务端端点已启动。LM Studio 默认 keyless；vLLM 由用户服务端配置决定是否设置 key。

## 替代方案

- 从 `api_style` 推断所有媒体功能：拒绝，因为 wire compatibility 不能证明服务实现专用媒体 endpoint。
- 将每个厂商定义为新的 `api_style`：拒绝，因为厂商身份与 wire protocol 是不同概念，会重复协议 adapter 并破坏共享的 vendor tuning。
- 只靠 UI 预设名称决定媒体能力：拒绝，因为用户可改 Base URL，且旧配置和自定义网关没有可靠的预设名称。

## 验证

- Common 单元测试覆盖旧配置缺省、安全 profile 推导、显式 opt-in、keyless 与无效 style/capability 组合。
- LLM 测试覆盖媒体 builder 对未声明 endpoint 的拒绝及显式兼容 endpoint 路由。
- UI 测试覆盖预设 identity、wire style 分离、Azure auth header、预设切换和保留用户自定义媒体能力。
- IPC contract 生成/漂移检查、Rust workspace tests/strict Clippy、UI type-check 与 tests。

## 回滚与重置

没有数据库迁移。如果旧 binary 需要读取已写入 `media_capabilities` 的配置，应先从 `config.toml` 的 provider 行移除该字段，或恢复当前版本。只使用新增预设且未保存媒体能力时无需数据重置。
