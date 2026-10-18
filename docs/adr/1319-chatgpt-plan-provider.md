# ADR 1319：将 ChatGPT 订阅登录作为独立 Provider

## 状态

已接受并实现（2026-10-18）。

## 背景

Haven 用户希望直接用 ChatGPT 订阅授权请求模型，并与 OpenAI API Key 的独立 API 计费区分。OpenAI 为开源、本地运行的应用提供 Sign in with ChatGPT 预览接入流程。它不使用 API key，也不使用 ChatGPT 私有 `backend-api`。

## 决定

- 增加独立 Provider 身份 `chatgpt-plan`，线协议使用现有 OpenAI Responses adapter，目标固定为 `https://api.openai.com/v1`。一般 OpenAI API Key Provider 保持原有认证和计费路径。
- 通过系统浏览器进行官方 OAuth 授权：随机 `state`、OIDC `nonce`、PKCE S256、精确的 `127.0.0.1` 回调；验证签名、issuer、audience、有效期、nonce 和 subject。OIDC 签名验证使用专用 JWT 库与官方 JWKS，不自写密码学实现。首次动态注册取得的 `client_id`、稳定 host ID、身份和访问/刷新令牌记录保存在 Windows 凭据管理器；ID token 验证后立即丢弃，配置只保留 `cred-*` 引用。凭据序列化在保存前限制为 Windows generic credential 的 2,560 字节上限。已签发的 `client_id` 用于之后登录和刷新，不保存客户端密钥；重新登录由用户在官方账户选择页选择账户，不把 ID token 或邮箱作为 OAuth URL hint。
- 登录身份与订阅用量权限分别处理。若 OAuth 返回有效身份但没有 `chatgpt.tokens.use.direct`，保留加密凭据并禁止推理；用户再次点击授权按钮时，使用原注册和完整 scope 请求 consent。模型调用在窄 auth port 内解析 `cred-*`，串行刷新轮换令牌，并且只向官方 API 发送 access token。
- 每个 Responses 请求强制 `stream: true`、`store: false`，提交完整本地历史；省略预览不支持的 token cap、temperature、top_p、缓存路由提示等字段。Haven 本地 function tools 放在 `haven` namespace，调用历史与流式调用名投影回 Haven 的规范名称；关闭内置 hosted search，不向该 Provider 声明 STT、TTS 或图片生成端点。只有明确的 `subscription_sharing_usage_limit_exceeded` 才作为不可重试的订阅用量错误；普通 429 保留瞬时速率限制处理，流开始后的错误不会静默切换到 API 计费路径。
- 身份列表调用官方 `/v1/models`，只展示 `visibility=list` 的模型并保留服务端顺序。

## 替代方案

- 把订阅凭据塞进普通 OpenAI API Key Provider：拒绝；会混淆两种计费授权，并可能将引用或订阅 token 送入通用 API-key header 流程。
- 调用 ChatGPT 私有 Web 后端：拒绝；使用未公开协议不属于支持的本地开源集成。
- 复用 API Key 做自动 failover：拒绝；订阅用量达到限制时不应在用户不知情的情况下转为 API 计费。

## 影响与数据

- 不修改数据库 schema，不需重置数据库。设置配置沿用现有 Provider 与 `api_key_ref` 契约；真实 token 只在 Windows 凭据管理器。删除 Provider 或切换认证类型会按既有 ConfigService 规则移除不再引用的凭据。
- ChatGPT plan 用量与 OpenAI API Key 的 API 计费分开；OpenAI 也可能对单个授权应用设置用量限制。收到用量限制错误不能直接推断整个订阅额度耗尽或重置时间，用户可到 ChatGPT Settings → Usage 查看。
- 这是预览集成，受 OpenAI 当前模型/账户政策与 Responses 限制约束；功能边界应随官方说明更新。

## 验证

- 单元测试覆盖 OAuth URL/PKCE/loopback 校验、state 错误拒绝、订阅授权缺失、凭据大小限制、凭据引用隔离、请求字段约束、本地工具 namespace 往返和订阅用量错误分类。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`：通过。
- UI `check`、生产构建、全量测试（161 个文件，1,341 项通过）及 IPC 事件/契约检查：通过。

## 回滚

移除 `chatgpt-plan` 预设、OAuth 命令与 LLM auth port 即可回滚。无需数据转换；现有通用 OpenAI API Key Provider 不受影响。删除计划 Provider 会由 ConfigService 清理关联凭据引用。
