# 本机工具安全回归矩阵

本矩阵覆盖 `haven-tools` 的 builtin 工具、MCP/skill 适配器和所有进入
`AuthorizationEngine` 的执行路径。风险级别的可执行代表行位于
`crates/tools/src/security.rs` 的 `LOCAL_TOOL_SECURITY_MATRIX`；这里补充每个 view 的
操作面、路径字段、授权继承、取消与竞态要求。模型可见的 operation-based builtin 统一使用
`root.operation`，聚合实现仅是 native/内部执行边界。矩阵中的“拒绝”是 fail-closed，
不得降级为确认或依赖前端传入的 `confirmed`。

## 风险与入口矩阵

| 工具 | 低风险/只读操作 | 需确认或更高风险操作 | 授权 key | 路径/外部边界 | 取消与重试 |
|---|---|---|---|---|---|
| `media.*`（音频设备分支） | `media.play`, `media.speak`, `media.volume_get`, `media.mute_get` | `media.record`, `media.volume_set`, `media.mute_set` = medium | view 的完整名称；父级 `media` 可作为 ToolConfig 家族设置 | `media.play.file_path` 与文件工具一样受 `allowed_paths` 约束；`media.speak.text` 发往已配置 TTS provider；录音设备由 input 管线管理 | 录音取消清理 recording id；TTS 合成支持取消，播放完成当前 WAV |
| `media.*`（内容派生） | `media.inspect`, `media.extract`, `media.render` = low | `media.describe`, `media.transcribe`, `media.generate` = medium；`media.ocr` = high | view 的完整名称；父级 `media` 可作为 ToolConfig 家族设置 | 仅接受受管 `asset_id`；OCR 只有专用 OCR client 存在时才暴露；视觉、STT、OCR 和生成分别经过实时 capability 与大小预算 | 派生调用可取消；provider 失败保留受管 asset reference，不暴露宿主路径 |
| `ask` | 全部 | 无系统副作用 = safe | `ask` | 无本地路径 | 不得静默跳过用户问题 |
| `files.*` | `files.read`, `files.inspect`, `files.stat`, `files.hash`, `files.list`, `files.outline`, `files.summary`, `files.search` = low | `files.write`, `files.edit`, `files.patch`, `files.copy`, `files.move`, `files.create_dir` = medium；`files.delete` = high；内容搜索 = medium | view 的完整名称；父级 `files` 可作为 ToolConfig 家族设置 | view 固定 operation，继承 `allowed_paths`；`path`, `paths`, `source`, `destination`, `file`, `dir`, `directory` 逐一校验；写入使用 expected_hash/CAS、原子替换和大小上限 | 失败不得部分放宽；dry-run 不产生写入；重试沿用同一 gate |
| `shell` | 无 | 所有命令 = high | `shell` | `cwd` 必须纳入路径校验；命令不通过 shell 拼接绕过 | 取消终止受管子进程；unsafe 重试默认关闭 |
| `system.*` / `process.*` / `clipboard.*` / `input.*` / `window.*` | `system.info`, `system.display`, `system.power.status`, `process.list`, `clipboard.read`, `clipboard.history`, `input.move`, `input.scroll`, `window.list`, `window.foreground`, `window.screenshot`, `window.ui_tree`, `window.observe`, `window.wait`, `system.env.get` = safe/low | `system.env.list/set/unset` = high（scope 明确为 process/user/machine）；`registry.delete_value`/其它写操作 = high，`registry.delete_key` = critical；`system.power.lock/sleep` = high、hibernate = critical；`process.kill` = high；`clipboard.write`、`input.click/type/key`、`window.invoke/set_value/toggle/select` = medium；`window.focus` = medium、`window.close/ocr` = high | view 的完整名称；各 family 父级可作为 ToolConfig 家族设置 | 每个 view 固定 scope/operation，分别沿用路径、桌面和设备边界；env list 只返回名称，credential-like get 脱敏且 set 不回显；clipboard rich read 只返回受管 asset_id；window 语义操作必须带近期稳定 target | 聚合实现只转发子工具策略；取消只允许在操作未提交前生效 |
| `http` | 无 | 请求 = medium | `http` | 默认阻断 localhost/loopback、私网、link-local、云元数据和解析到受限地址的域名；可用 `allowed_domains` 进一步收窄；每个 redirect hop 重新校验，跨 origin 移除认证/cookie 头 | timeout/cancel 后不得自动升级重试；未知结果不重放 |
| `notify` | 全部 = safe | 无 | `notify` | UI 文本按纯文本处理 | 重复通知可丢弃/幂等 |
| `agent.*` | `agent.list`, `agent.children`, `agent.history`, `agent.profile`, `agent.inbox`, `agent.ack`, `agent.reply`, `agent.request`, `agent.status`, `agent.join`, `agent.wait`, `agent.collect`, `agent.send` = safe | `agent.spawn` = medium；`agent.stop` = high | view 的完整名称；父级 `agent` 可作为 ToolConfig 家族设置 | peer bus 路径固定在受管 root；lifecycle/history target 只允许当前 session 或其后代 | inbox 默认 claim 不 ack，必须在 durable project/snapshot 后用 message id 或 claim token 显式 ack；request/join/wait 等待取消必须释放 waiter；stop 复用 SessionExecutor 取消与清理 |
| `load_mcp` | 加载元数据 = safe | 被加载 MCP 工具统一按 high gate | `load_mcp` | MCP 配置/env 不进入普通错误或 UI | 连接取消必须关闭 client |
| `memory.*` | `memory.search`, `memory.list`, `memory.recall` = safe | `memory.remember`, `memory.forget` = medium | view 的完整名称；父级 `memory` 可作为 ToolConfig 家族设置 | 事实写入拒绝 credential-like 值 | maintenance/embedding 操作支持取消或有界执行 |
| `haven.*`、`actions.*`、`schedule.*`、`preferences.*`、`checklist.*` | 诊断、配置读取、技能/工具/MCP 列表、`actions.list/inspect`、`schedule.list`、`preferences.get/list`、`checklist.list` = safe/low | `haven.config.logs_level`、技能/工具 enable/disable、MCP connect/disconnect/reload、`actions.cancel` = medium；skill_create、MCP add/update/toggle/remove = high；schedule/preferences/checklist 写操作沿用各自 view 风险 | view 的完整名称；各 family 父级可作为 ToolConfig 家族设置 | 每个 view 使用独立 schema、风险、并发资源和 session 归属；配置读取递归脱敏，MCP env 不返回，任务取消按 session 校验 | 聚合实现只转发子工具策略；保存失败不产生半更新状态，诊断失败不得暴露原始日志或会话正文 |

### 入口一致性

| 入口 | 必须执行的检查 | 回归断言 |
|---|---|---|
| ReAct 工具执行 | `AgentExecutor::execute_gated` → `AuthorizationEngine::check` → 执行 | 未获批不能调用 tool；执行前再次检查以覆盖 TOCTOU |
| 定时任务触发 | 设定时按 registry 风险检查，触发时再次经过 executor gate | 设定时的允许不能替代触发时的当前拒绝 |
| MCP 适配器 | `mcp__server__tool` 使用 adapter 的 high 风险和同一授权 key | UI 预览与 Agent 调用共享 permanent grant；session grant 不泄漏到无 session 入口 |
| skill 适配器 | `skill__name` 使用 adapter 的 high 风险和同一授权 key | skill 脚本不能由 `confirmed` 参数绕过 deny/path gate |
| UI MCP/skill 命令 | `mcp_tool_call` / `execute_skill` 先检查 gateway | 被拒绝时不创建 client call/runner call |
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
| policy reset | 修改阈值、重新加载安全配置、clear history | 清除旧 session grants，不残留信任 |
| disabled op | `disabled_operations` 命中 op/scope/scope:op | `Blocked`，优先于风险确认 |
| operation view contract | view 的 schema、固定 operation/scope、风险、幂等性、并发、权限 key、renderer、icon、prompt 任一不一致 | catalog、AuthorizationEngine、UI parser/renderer 和 prompt 不得各自接受不同定义 |
| HTTP destination | `http://localhost`, loopback、RFC1918/ULA、link-local、`169.254.169.254`、metadata hostname | `Blocked`，不能依赖代理或 DNS 结果把本地目标变成可访问目标 |
| HTTP redirect | 公网 URL 重定向到上述地址，或超过 10 跳 | `Blocked`；自动跟随必须关闭，逐跳解析、allowlist 和地址检查 |
| HTTP allowlist | 配置 `allowed_domains` 后访问未列出的 host、或 `*.example.com` 访问根域 | `Blocked`；通配符只匹配子域，不扩大到根域或其他后缀 |
| structured error class | `transient`、`unknown_outcome`、`validation`、`permission`、`side_effect_may_have_happened` | agent 按 class 决定 retry/verify/ask；不得把错误文本作为主要策略条件 |
| cancellation | gate 后、实际文件/进程/网络提交前取消 | 不调用下游，或下游按 token 停止；不以取消作为授权 |
| race/TOCTOU | check 后替换路径 component 为 reparse point | 执行入口重新 check；发现不一致即拒绝并记录净化错误 |
| 输出泄漏 | denial、tool error、日志 tail、MCP env | 不返回 key/token、完整命令输出或原始 provider/MCP secret |

以上矩阵由 `haven-tools` 的 AuthorizationEngine 单元测试覆盖核心判定；文件系统
重解析点测试在 Unix 使用 symlink fixture，Windows 使用同一实现的
`FILE_ATTRIBUTE_REPARSE_POINT` 检测路径编译验证。真实用户目录、注册表、网络、
电源和桌面输入不属于自动化 fixture 的目标。

