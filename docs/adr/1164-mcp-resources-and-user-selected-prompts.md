# ADR 1164：MCP Resources 与用户选择的 Prompts

状态：已接受，分垂直切片实施。

## 背景

Haven 当前 MCP client 只协商并缓存 `tools` capability。MCP 2026-07-28 将 Resources 和 Prompts 定义为独立 server capability：Resources 由 host 决定何时读取，Prompts 应由用户显式选择。列表使用 cursor 分页；列表和读取结果都可带 `ttlMs` 与 `cacheScope`。`resources/read`、`prompts/get` 还可能返回 `input_required`，等待一次或多次额外交互。

规范：[Resources](https://modelcontextprotocol.io/specification/2026-07-28/server/resources)、[Prompts](https://modelcontextprotocol.io/specification/2026-07-28/server/prompts)、[Base Protocol](https://modelcontextprotocol.io/specification/2026-07-28/basic)。

## 决定

1. **保持 host-driven 与 user-selected 语义。** Haven 通过工具页列出已启用 server 提供的资源和 prompt。用户选中资源后才调用 `resources/read`；用户选中 prompt 并提交参数后才调用 `prompts/get`。这些能力不作为模型可自行遍历 URI、拉取资源或触发 prompt 的通用 MCP tool 暴露。
2. **以 MCP connection owner 管理能力、缓存和路由。** 每个 `McpClient` 按实际协商的 modern/legacy server capability 决定是否允许 list/read/get/subscribe。Manager 操作按 server 名和当前 connection target binding 路由；重连或替换后，旧请求必须 fail closed。会话 ID 不是 MCP transport session，也不参与缓存共享。
3. **分页和响应使用硬上限。** List API 校验 cursor 非空、长度有界且不循环；限制页数和聚合条目数。资源读取限制内容条数、URI/MIME 长度、文本字节、Base64 字节和总响应大小；prompt 列表、参数定义、消息条数、文本和二进制内容也有总预算。越界结果整体拒绝，不截成可被误认作完整内容的片段。
4. **按连接代次缓存且服从 server hint。** `ttlMs` 决定条目过期时间，`cacheScope` 保留在 typed result 并禁止跨 server/client 共享。缓存只存在于当前 `McpClient` 连接代次的进程内；target 变化、reconnect、显式刷新或过期使其失效。仅用户显式订阅的 URI 收到匹配更新通知后，立即失效该 URI 的读取缓存；资源列表和 prompt 列表仍靠 TTL 或显式刷新。不得把资源正文写进 Memory、配置、日志或通用持久缓存。
5. **URI 是不透明的外部身份。** Haven 只把 URI 传给被选中的、仍符合配置和 target binding 的 MCP server；不得将 `file://` 解释为本地文件、不得将 `https://` 自动转成 Haven 的 HTTP fetch，也不得从资源描述推导 sandbox/workspace 路径。响应正文属于不可信 server 内容，UI 显示来源 server、URI 和 MIME。
6. **交互请求先 fail closed。** 在 Haven 建立可取消、可恢复且有明确 UI owner 的 MRTR 生命周期前，收到 `input_required` 时返回结构化的“需要额外交互，当前未执行”错误；不丢弃 request state 后重试，不猜测或自动补值。该行为对现代和旧协议版本一致。
7. **Prompt 内容须由用户启动。** `prompts/list` 只供 UI 发现。`prompts/get` 结果经过 typed parse 与大小检查后，由用户预览并显式加入聊天草稿；Composer 保持编辑权，只有之后普通的用户发送操作才会进入会话。服务端 prompt 文本、资源和多模态块都视为不可信输入；不自动执行工具、脚本或资源读取。首期只有 Haven 能完整表示且在预算内的内容可以加入草稿；未知类型和超限内容整体 fail closed。
8. **不改变既有 transcript 权威。** 用户提交的 prompt 内容或资源摘录按普通用户提交进入现有 ingress 和 `session_events` 路径；MCP list/read/get 的过程状态不额外造 transcript 气泡或数据库表。持久化后仍按 X12 和 rollback 双时钟规则处理。

## 安全与失败语义

- 未声明 capability 的 server 不会收到相应 RPC；disabled server、断开的连接、target binding 变化、无效 URI、非法 cursor、未知结果类型、`input_required`、非法 content shape 和预算超限均显式失败。
- list/read/get 通过同一连接的 `McpClientInner::request`，沿用既有请求超时、网络策略、cancel、敏感错误脱敏和 HTTP 路由限制。资源订阅使用同一 target-bound `McpClient` 上独立的 `subscriptions/listen` 流；stdio 发送 `notifications/cancelled`，HTTP 关闭对应 SSE 响应体。
- `cacheScope: public` 不授权跨 Haven server 配置共享缓存。两个配置即使指向相同 URL，也有独立 client、凭据和生命周期。
- 这项能力不提供本地 filesystem sandbox；MCP server 内部如何解析 URI 仍由 server 负责，但客户端不会替它读取本机路径。

## 实施顺序

1. 增加 modern/legacy Resources 与 Prompts capability snapshot、typed DTO、分页/内容校验、缓存与 target-bound Manager API。
2. 增加用户可见的资源浏览/读取和 prompt 选择/预览/提交路径；命令仅接受当前 server 名、URI 或 prompt 名及参数，不接受任意 URL 或路径执行指令。
3. 完成协议正反例、能力缺失、target 替换、分页循环、缓存过期、输入交互 fail-closed、内容预算及 IPC/UI 端到端验收。
4. 之后再评估 MRTR 生命周期。只有证明需要跨用户交互或进程重启时才接入既有 durable interaction/session event 机制；不另建 prompt job、cache database 或恢复 owner。

## 首期实现边界

- 已接通 modern/legacy capability 协商、分页 `resources/list` / `prompts/list`、用户选中的 `resources/read` / `prompts/get`、连接内 hint-aware cache 和 target-binding IPC。缓存留在单个 client 连接代次内；资源读取缓存还会在已订阅 URI 收到更新通知时失效。资源列表和 prompts 仍没有通知订阅。
- `resources/templates/list` 已按相同的分页、总量、TTL/cacheScope 与 target-binding 规则接通；工具页允许用户选择模板、填写 RFC 6570 字符串变量、预览展开后的 URI，再由用户发起 `resources/read`。展开逻辑支持 RFC 6570 字符串变量、操作符与前缀修饰；模板名/URI 均保持为不透明的 MCP server 数据。
- `resources.subscribe=true` 时，用户可以对当前资源列表中的 URI 开启或取消 `subscriptions/listen` 订阅。每个流必须确认包含同一 URI 的 acknowledgment；最多 64 个并发资源订阅。只有匹配活动 `subscriptionId` 和 URI 的更新才驱逐对应读取缓存并通知 UI；UI 标记资源已更新，等待用户手动重新读取。关闭浏览对话框或连接关闭时取消流；订阅状态仅在连接内有效，不持久化，也不跨重连恢复。服务端不接受过滤项时返回失败，不回退到未过滤监听。
- UI 只对启用且连接中的服务器显示浏览入口。资源正文和 prompt 结果以纯文本预览；二进制内容只显示类型与大小信息，非纯用户文本不能加入聊天草稿。资源/prompt 都不作为模型工具或 CodeMode 子调用公开。
- 本切片不实现 MRTR，也不自动订阅资源列表或 prompts 变化通知；这些能力不得被 UI 或模型假定为可用。MRTR 仍需独立的可取消且有明确 UI owner 的生命周期。
- Resources 与 Prompts 不持久化。资源文本或 prompt 用户消息仅在用户显式加入草稿并随后发送后，才经过普通 ingress 和既有 transcript/event owner。

## 拒绝的方案

- 将所有 resources 塞进 system prompt：目录可能很大、内容可能敏感且会过期，也违背应用驱动的资源选择方式。
- 暴露一个模型任意指定 URI 的 `read_resource` tool：它让模型主动查询用户未选择的数据，并容易把 MCP URI 混同本地路径或网络 URL。
- 用“忽略 `input_required` 后重试”处理 MRTR：后续请求缺少 server 要求的 inputResponses/requestState，且可能重复外部操作。
- 将 `https://` resource 直接交给通用 HTTP fetch：这会绕过 MCP server 的授权上下文和 Haven 已有网络安全目标校验。
- 持久化所有 resource/prompt 响应以便缓存：缓存 scope、授权身份、server 内容更新与用户数据删除语义都不匹配通用数据库。

## 验收

- modern/legacy 按 capability 独立识别；缺失 capability 时无 RPC。
- `resources/list`、`prompts/list` 多页读取正常；重复、空、超长 cursor 和页数/总量超限被拒绝。
- `resources/read` 验证 complete/text/blob、多内容响应、URI 与所有字节上限；`input_required` 和未知类型 fail closed。
- 资源订阅只允许对已声明 `subscribe` capability 且当前资源列表中的 URI 开放；acknowledgment 必须接受精确 URI，更新通知必须匹配活动 `subscriptionId`，取消会结束对应 stdio/HTTP 流，超出并发上限时拒绝。
- `prompts/get` 验证 required arguments、角色、text/image/audio/resource/resource_link 内容、聚合字节上限和 `input_required`。
- cache TTL/public-private hint、显式刷新、reconnect/target replacement 和过期失效均有测试；资源更新订阅仅作用于用户选择的 URI，资源列表/prompts 的 list-changed 通知仍未开放。
- IPC/UI 只读取当前 enabled server，并对 resource 与 prompt 内容展示来源；提交后的内容经过普通用户 ingress。
- 无数据库或配置持久化变更，不需要重置既有用户数据。
