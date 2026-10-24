# 本机工具安全回归矩阵

本矩阵覆盖 `haven-tools` 的 builtin 工具、MCP/skill 适配器和所有进入
`AuthorizationEngine` 的执行路径。风险级别的可执行代表行位于
`crates/tools/src/security.rs` 的 `LOCAL_TOOL_SECURITY_MATRIX`；这里补充每个 view 的
操作面、路径字段、授权继承、取消与竞态要求。模型可见的 operation-based builtin 统一使用
`root.operation`，聚合实现仅是 native/内部执行边界。矩阵中的“拒绝”是 fail-closed，
不得降级为确认或依赖前端传入的 `confirmed`。

`ReadOnly` / `WorkspaceWrite` 的文件路径边界只作用于声明为
`OperationEffect::WorkspaceWrite` 的本地文件写操作；其它 `ExternalEffect` 仍按各自的执行边界、
网络策略、风险、权限和确认规则处理。`WorkspaceWrite` 下 Shell 优先通过 MXC BaseContainer；
不支持时可降至 MXC AppContainer 兼容层（BFS 或 DACL），使用 DACL 时授权根 ACL 临时调整并在进程结束时恢复。兼容层将允许网络
收窄为仅公网出站，拒绝私有网络入站与 host loopback；拒绝网络仍保持拒绝。原始请求和兼容请求都无法验证时 fail closed，
绝不回退宿主进程。降级通过 Shell 结果向用户说明。Skill、MCP stdio 及其它未接入隔离 capability 仍拒绝。
回归覆盖外链目标位于 writable roots 之外、`NetworkPolicy::Restricted` 时，ReadOnly 与
WorkspaceWrite 都进入确认流程；本地文件写入仍分别受只读拒绝与工作区根限制。

## 风险与入口矩阵

| 工具 | 低风险/只读操作 | 需确认或更高风险操作 | 授权 key | 路径/外部边界 | 取消与重试 |
|---|---|---|---|---|---|
| `media.*`（音频设备分支） | `media.play`, `media.speak`, `media.get_volume`, `media.get_mute` | `media.record`, `media.set_volume`, `media.set_mute` = medium | view 的完整名称；父级 `media` 可作为 ToolConfig 家族设置 | `media.play.file_path` 与文件工具一样受 `allowed_paths` 约束；`media.speak.text` 发往已配置 TTS provider；录音设备由 input 管线管理 | 录音取消清理 recording id；TTS 合成支持取消，播放完成当前 WAV |
| `media.*`（内容派生） | `media.inspect`, `media.extract`, `media.render` = low | `media.describe`, `media.transcribe`, `media.generate` = medium；`media.run_ocr` = high | view 的完整名称；父级 `media` 可作为 ToolConfig 家族设置 | 仅接受受管 `asset_id`；OCR 只有专用 OCR client 存在时才暴露；视觉、STT、OCR 和生成分别经过实时 capability 与大小预算 | 派生调用可取消；provider 失败保留受管 asset reference，不暴露宿主路径 |
| `ask` | 全部 | 无系统副作用 = safe | `ask` | 无本地路径 | 不得静默跳过用户问题 |
| `files.*` | `files.read`, `files.inspect`, `files.list`, `files.get_outline`, `files.summarize`, `files.search` = low | `files.write`, `files.patch`, `files.copy`, `files.move`, `files.create_directory` = medium；`files.delete` = high；内容搜索 = medium | view 的完整名称；父级 `files` 可作为 ToolConfig 家族设置 | view 固定 operation，继承 `allowed_paths`；`path`, `paths`, `source`, `destination`, `file`, `dir`, `directory` 逐一校验；写入使用 expected_hash/CAS、原子替换和大小上限；write 可在获准目标下创建父目录，dry-run 与 CAS 失配不创建目录；delete 只删除文件或空目录，不递归 | 失败不得部分放宽；dry-run 不产生写入；重试沿用同一 gate |
| `shell` | 无 | 所有命令 = high | `shell` | `FullAccess` 沿用当前 Windows 用户权限；`WorkspaceWrite` 优先 MXC BaseContainer，兼容时可降至 AppContainer tier，只有有效可写根可写，系统运行时/PATH/cwd 可只读；降级层仅允许公网出站并显示提示；无兼容 tier 时拒绝，绝不回退宿主进程 | 取消或授权撤销终止 MXC/Job Object 进程树；DACL 随子进程结束恢复；输出保持有界；unsafe 重试默认关闭 |
| `system.*` / `clipboard.*` / `input.*` / `window.*` | `system.get_info`, `system.list_displays`, `system.power.get_status`, `clipboard.read`, `clipboard.list_history`, `input.move`, `input.scroll`, `window.list`, `window.get_foreground`, `window.capture_screenshot`, `window.get_ui_tree`, `window.observe`, `window.wait`, `system.env.get` = safe/low | `system.env.list` = high（scope 明确为 process/user/machine）；注册表只读 = medium；`system.power.lock/sleep` = high、hibernate = critical；`clipboard.write`、`input.click/type/key/click_element/type_element`、`window.invoke/set_value/toggle/select` = medium；`window.focus` = medium、`window.close` = high | view 的完整名称；各 family 父级可作为 ToolConfig 家族设置 | 每个 view 固定 scope/operation，分别沿用路径、桌面和设备边界；`system.env.list` 只返回名称并用 `prefix` 过滤，credential-like get 脱敏；clipboard rich read 只返回受管 asset_id；window 语义操作必须带近期稳定 target | 聚合实现只转发子工具策略；取消只允许在操作未提交前生效 |
| `http` | 无 | 请求 = medium | `http` | 默认阻断 localhost/loopback、特殊用途和解析到受限地址的域名；可用 `allowed_domains` 进一步收窄；公网地址固定到连接；关闭代理和自动重定向；仅允许逐跳复核的 same-origin redirect；URL/header/body 有字节或条目上限 | 在途取消丢弃 request future；timeout/cancel 后不得自动升级重试；未知结果不重放 |
| OpenAI 图片 URL 下载 | 无 | 由已授权图片生成操作覆盖 | provider 返回的 `url` | 不带凭据/cookie；不使用代理；关闭自动重定向，每跳重新解析、阻断私有/特殊用途地址并固定公网 IP；允许受校验的公网 CDN 跨 origin redirect | 最多 10 跳，响应最多 32 MiB；取消/超时释放当前请求 |
| `notify` | 全部 = safe | 无 | `notify` | UI 文本按纯文本处理 | 重复通知可丢弃/幂等 |
| `session.*` | `session.list_links` = safe；`session.link`、`session.unlink` = low | `session.create` = medium | view 的完整名称；父级 `session` 可作为 ToolConfig 家族设置 | 创建只允许从注入的当前 session 建立 parent lineage；关联只能连接当前 session 与一个 canonical `related_session_id`，两端必须存在 | 创建受父 session 的 child 数上限约束；关联幂等，查询和删除任一端都可见 |
| `agent.*` | `agent.list`, `agent.list_children`, `agent.read_history`, `agent.get_profile`, `agent.list_inbox`, `agent.acknowledge`, `agent.reply`, `agent.request`, `agent.get_status`, `agent.wait`, `agent.collect`, `agent.send` = safe | `agent.stop` = high | view 的完整名称；父级 `agent` 可作为 ToolConfig 家族设置 | peer bus 路径固定在受管 root；lifecycle/history target 只允许当前 session 或其后代 | inbox 默认领取不确认；只有消息已进入 durable transcript/event 投影后，才能按 `message_ids` 显式 ack；`claim_token` 不再受支持。request/wait 等待取消必须释放 waiter；stop 复用 SessionSupervisor / SessionActor 取消与清理 |
| `load_mcp` | 加载元数据 = safe | 被加载 MCP 工具统一按 high gate | `load_mcp` | MCP 配置/env 不进入普通错误或 UI | 连接取消必须关闭 client |
| `load_skill` | 加载技能元数据 = safe | 被加载 `skill__name` 执行适配器统一按 high gate | `load_skill` / `skill__name` | 只加载发现后的技能描述、manifest 与可执行状态，不执行脚本 | 执行通过 SkillRunner 的进程隔离、总期限与取消边界 |
| `memory.*` | `memory.search`, `memory.list`, `memory.history`, `memory.valid_at`, `memory.recall` = safe | `memory.remember`, `memory.forget` = medium | view 的完整名称；父级 `memory` 可作为 ToolConfig 家族设置 | 事实写入拒绝 credential-like 值；有效时间必须显式提供且区间格式有效 | maintenance/embedding 操作支持取消或有界执行 |
| `haven.*`、`tool_runs.*`、`schedule.*` | 诊断、配置读取、技能/工具/MCP 列表、`tool_runs.list/inspect`、`schedule.list` = safe/low | `haven.config.set_log_level`、技能/工具 enable/disable、MCP connect/disconnect/reload/reconnect/refresh、`tool_runs.cancel` = medium；skill_create、MCP add/update/toggle/remove = high；schedule 写操作沿用各自 view 风险 | view 的完整名称；各 family 父级可作为 ToolConfig 家族设置 | 每个 view 使用独立 schema、风险、并发资源和 session 归属；技能启用和创建必须有脚本入口；配置读取递归脱敏，MCP env 不返回，任务取消按 session 校验 | 聚合实现只转发子工具策略；保存失败不产生半更新状态，诊断失败不得暴露原始日志或会话正文 |

### 入口一致性

| 入口 | 必须执行的检查 | 回归断言 |
|---|---|---|
| ReAct 工具执行 | `AgentExecutor::execute_gated` → `AuthorizationPort` decision/proof → `AuthorizedExecutor` 每次 attempt 复核 → 执行 | auto proof 绑定 session/tool/capability/input/target、operation policy 与 policy revision；confirmation 绑定 receipt；跨 child capability 或 session 的 proof 被拒绝；attempt gate 重检当前 deny、sandbox/network 与 revision，失败时不进入 handler；retry 前再次检查；handler 已开始后的撤销不回滚已发起的外部副作用 |
| 定时任务触发 | 设定时按 registry 风险检查，触发时再次经过 executor gate | 设定时的允许不能替代触发时的当前拒绝 |
| MCP 适配器 | `mcp__server__tool` 使用 adapter 的 high 风险和同一授权 key；proof 绑定 manager/client target identity，manager 路由前再次比较 | UI 预览与 Agent 调用共享 permanent grant；session grant 不泄漏到无 session 入口；client/manager 替换后旧确认不被重定向 |
| skill 适配器 | `skill__name` 使用 adapter 的 high 风险和同一授权 key | skill 脚本不能由 `confirmed` 参数绕过 deny/path gate |
| UI Skill/MCP 管理命令 | `execute_skill` 先检查 gateway；`reconnect_mcp_server` 对一个已启用 server 授权；`refresh_mcp_servers` 对配置 diff 的 affected set 授权一次 | Skill 未授权时不执行；MCP 未授权/待确认期间不创建、连接、重连或断开 client；`NetworkPolicy::Deny` 在连接前阻断，`Ask` 要求确认；确认后在共享配置 gate 内复核 config version 与完整 target set |
| 自身设置入口 | Tauri 设置命令复用 native admin surface，并先经过 AuthorizationEngine | UI 与模型 capability 使用同一 typed 写路径；确认恢复仍绑定 receipt，native façade 为临时迁移边界 |

## 负向回归矩阵

| 类别 | 输入/状态 | 期望 |
|---|---|---|
| 规范化 | `allowed/../outside`、`.`、重复分隔符 | 规范化后越界立即 `Blocked` |
| 重解析点 | allowed root 下的 symlink/junction/reparse component 指向 root 外 | 任一现存 component 是 reparse point 即 fail-closed；不得只比较 lexical prefix |
| 最终重解析点 | 目标本身是 symlink/reparse point | `Blocked`，包括目标尚不存在但父路径经过 reparse point 的情况 |
| 源路径 | `files.copy/move` 的 `source` 在 root 内、`destination` 在 root 外 | `Blocked` |
| 目标路径 | source 在 root 外、destination 在 root 内 | `Blocked` |
| 多路径数组 | `paths` 中任一项越界、相对或 reparse | 整个调用 `Blocked`，不部分执行 |
| 相对路径 | `file.txt`、相对 `cwd` | `Blocked`；含义不得随进程 CWD 改变 |
| UNC/device | `\\server\share`, `//server/share`, `\\?\`, `\\.\` | `Blocked`，除非另有明确、受管的本机策略；当前策略统一拒绝 |
| 授权继承 | parent allow → child operation | 允许，且只覆盖该工具父树 |
| 授权继承 | child allow + parent deny | `Blocked`，deny 扫描整条 ancestry |
| 授权继承 | child deny + parent allow | `Blocked` |
| 授权优先级 | permanent deny + session allow | `Blocked` |
| 会话隔离 | session allow 在 `ses-a`，无 session 或 `ses-b` 调用 | 后两者不能自动批准 |
| 确认后 MCP refresh 部分失败 | queued refresh 被批准，结果 `failed` 含本批 reconnect target | resolver 保留 `ToolResult`，只经既有 `mcp:status_change` 为失败 target 发通用 `Offline`；direct command 仍只用 response DTO 汇总，不重复发失败事件 |
| MCP refresh 失败 payload | `failed` 为空、不是数组、包含非字符串/空项/计划外名称/重复名称 | 不发 Offline；只发布唯一且匹配授权 connect/reconnect target 的名称，不向 renderer 暴露底层连接错误 |
| policy reset / session grants | live Security 配置 apply、`reset_permissions`、`reset_session_permissions`、永久与会话逐项撤销、history clear、retention cleanup | Security apply 清空 RAM map 后从 durable session rows 恢复；永久撤销不影响 session grants；会话撤销精确到 session+capability；两个 reset 分别保留另一类规则；会话删除/retention 由外键清理；rollback 不影响授权 |
| disabled op | `disabled_operations` 命中 canonical capability 或工具设置名下的短写；即使原始 discriminator 不一致也按 capability 判断 | `Blocked`，优先于风险确认 |
| operation view contract | view 的 schema、固定 operation/scope、风险、幂等性、并发、权限 key、renderer、icon、prompt 任一不一致 | catalog、AuthorizationEngine、UI parser/renderer 和 prompt 不得各自接受不同定义 |
| HTTP destination | `http://localhost`, loopback、RFC1918/ULA、link-local、`169.254.169.254`、metadata hostname | `Blocked`，不能依赖代理或 DNS 结果把本地目标变成可访问目标 |
| HTTP redirect | 公网 URL 重定向到上述地址、跨 origin，或超过 10 跳 | `Blocked`；自动跟随必须关闭，逐跳解析、allowlist 和地址检查；HTTP 工具只接受 same-origin redirect |
| Image URL redirect | provider 返回的公网图片 URL 重定向到 localhost/private/link-local/metadata 或超过 10 跳 | `Blocked`；每跳 DNS 全量检查并固定目的地址；不得携带 provider credential/cookie |
| HTTP allowlist | 配置 `allowed_domains` 后访问未列出的 host、或 `*.example.com` 访问根域 | `Blocked`；通配符只匹配子域，不扩大到根域或其他后缀 |
| structured error class | `transient`、`unknown_outcome`、`validation`、`permission`、`side_effect_may_have_happened` | agent 按 class 决定 retry/verify/ask；不得把错误文本作为主要策略条件 |
| cancellation | gate 后、实际文件/进程/网络提交前取消 | 不调用下游，或下游按 token 停止；不以取消作为授权 |
| race/TOCTOU | 文件读授权后、打开前替换目标或路径 component；打开后再替换 pathname | 文件读入口先打开并持有 handle，再解析路径、验证允许根和 handle/file identity；无法证明相同对象时 fail-closed；读取、摘要、outline、inspect/hash、内容搜索及受管媒体读取都消费已验证的同一 handle。仅在执行入口重查 pathname 不足以满足此项 |
| 输出泄漏 | denial、tool error、日志 tail、MCP env | 不返回 key/token、完整命令输出或原始 provider/MCP secret |

以上矩阵由 `haven-tools` 的 AuthorizationEngine 单元测试覆盖核心判定；文件系统
重解析点测试在 Unix 使用 symlink fixture，Windows 使用同一实现的
`FILE_ATTRIBUTE_REPARSE_POINT` 检测路径编译验证。真实用户目录、注册表、网络、
电源和桌面输入不属于自动化 fixture 的目标。

