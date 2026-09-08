# 本机工具安全回归矩阵

本矩阵覆盖 `haven-tools` 的 builtin 工具、MCP/skill 适配器和所有进入
`SafetyGateway` 的执行路径。风险级别的可执行代表行位于
`crates/tools/src/security.rs` 的 `LOCAL_TOOL_SECURITY_MATRIX`；这里补充每个工具的
操作面、路径字段、授权继承、取消与竞态要求。矩阵中的“拒绝”是 fail-closed，
不得降级为确认或依赖前端传入的 `confirmed`。

## 风险与入口矩阵

| 工具 | 低风险/只读操作 | 需确认或更高风险操作 | 授权 key | 路径/外部边界 | 取消与重试 |
|---|---|---|---|---|---|
| `audio` | `play`, `volume_get`, `mute_get` | `record`, `volume_set`, `mute_set` = medium | `audio` 或操作 key | `play.file_path` 与文件工具一样受 `allowed_paths` 约束；录音设备由 input 管线管理 | 录音取消清理 recording id |
| `ask` | 全部 | 无系统副作用 = safe | `ask` | 无本地路径 | 不得静默跳过用户问题 |
| `files` | `read`, `list`, 普通 `search` | `write`, `edit`, `copy`, `move`, `create_dir` = medium；`delete` = high；内容搜索 = medium | `files`, `files:<operation>` | `path`, `paths`, `source`, `destination`, `file`, `dir`, `directory`；源和目标逐一校验 | 失败不得部分放宽；重试沿用同一 gate |
| `process` | `list` = low | `kill` = high | `process`, `process:<operation>` | 无路径参数；进程启动统一走 `shell.background` | kill 支持 token；取消不得继续执行 |
| `clipboard` | `read`, `history` = low | `write` = medium | `clipboard`, `clipboard:<operation>` | 无路径；文本长度受限 | 失败不重放写入 |
| `shell` | 无 | 所有命令 = high | `shell` | `cwd` 必须纳入路径校验；命令不通过 shell 拼接绕过 | 取消终止受管子进程；unsafe 重试默认关闭 |
| `actions` | 列表/查看 = safe | `cancel` = medium | `actions`, `actions:cancel` | 仅本地任务投影；取消按 session 归属校验 | 任务取消必须幂等 |
| `input` | `move`, `scroll` = low | `click`, `type`, `key` = medium | `input`, `input:<operation>` | 无路径 | 取消不得继续发送输入事件 |
| `schedule` | `list`, `cancel` = safe | `set` = low | `schedule`, `schedule:set` | 被调工具在设定时校验风险，触发时再次 gate | 定时任务取消和会话结束都必须阻断后续触发 |
| `system` | `info`, `display`, `power:status` = safe | env 写操作、registry 写操作 = high；registry 读 = medium；power lock/sleep = high；hibernate = critical | `system:<scope>[:operation]` | env list 只返回名称，credential-like get 脱敏且 set 不回显；注册表、电源、环境变量不接受路径绕过 | 取消只允许在操作未提交前生效 |
| `window` | `list`, `foreground`, `screenshot`, `ui_tree`, `wait` = low | `focus` = medium；`close`, `ocr` = high | `window`, `window:<operation>` | focus/close 可按 title 或 pid 定位；OCR 上传前仍需 high gate；不得泄漏完整屏幕到错误文案 | wait/ocr 支持取消，不得后台继续轮询 |
| `http` | 无 | 请求 = medium | `http` | URL/headers/body 是网络边界；不把 HTTP 当作本地路径 | timeout/cancel 后不得自动升级重试 |
| `notify` | 全部 = safe | 无 | `notify` | UI 文本按纯文本处理 | 重复通知可丢弃/幂等 |
| `agent` | list/profile/mail/poll = safe | `spawn` = medium | `agent`, `agent:spawn` | peer bus 路径固定在受管 root | request 等待取消必须释放 waiter |
| `load_skill` | 加载元数据 = safe | 被加载 skill 的工具另行 high gate | `load_skill` | skill root 由 engine 固定 | 失败不留下半注册工具 |
| `load_mcp` | 加载元数据 = safe | 被加载 MCP 工具统一按 high gate | `load_mcp` | MCP 配置/env 不进入普通错误或 UI | 连接取消必须关闭 client |
| `memory` | search/list/recall = safe | remember/forget = medium | `memory`, `memory:<operation>` | 事实写入拒绝 credential-like 值 | maintenance/embedding 操作支持取消或有界执行 |
| `haven_diagnostics` | status、logs_tail、sessions、errors = low | 无 | `haven_diagnostics`, `haven_diagnostics:<operation>` | 日志只返回脱敏、截断内容；会话诊断只返回元数据和字符数 | 诊断失败不得暴露原始日志或会话正文 |
| `haven_config` | config_get = low | logs_level = medium | `haven_config`, `haven_config:<operation>` | 配置读取递归脱敏；写入只接受 typed patch | 保存失败不得留下半更新状态 |
| `haven_skills` | skills_list = low | enable/disable = medium；create = high | `haven_skills`, `haven_skills:<operation>` | 技能 root 由 engine 固定，脚本大小受限 | 创建或保存失败必须回滚可见状态 |
| `haven_tools` | 无 | enable/disable = medium | `haven_tools`, `haven_tools:<operation>` | 只改变 allowlisted builtin tool 设置 | 保存后重建 catalog，失败不产生半更新 |
| `haven_mcp` | mcp_list = low | connect/disconnect/reload = medium；add/update/toggle/remove = high | `haven_mcp`, `haven_mcp:<operation>` | MCP env 值不返回；外部连接错误净化 | 配置与 client 状态保持一致，失败回滚 |

### 入口一致性

| 入口 | 必须执行的检查 | 回归断言 |
|---|---|---|
| ReAct 工具执行 | `AgentExecutor::execute_gated` → `SafetyGateway::check` → 执行 | 未获批不能调用 tool；执行前再次检查以覆盖 TOCTOU |
| 定时任务触发 | 设定时按 registry 风险检查，触发时再次经过 executor gate | 设定时的允许不能替代触发时的当前拒绝 |
| MCP 适配器 | `mcp__server__tool` 使用 adapter 的 high 风险和同一授权 key | UI 预览与 Agent 调用共享 permanent grant；session grant 不泄漏到无 session 入口 |
| skill 适配器 | `skill__name` 使用 adapter 的 high 风险和同一授权 key | skill 脚本不能由 `confirmed` 参数绕过 deny/path gate |
| UI MCP/skill 命令 | `mcp_tool_call` / `execute_skill` 先检查 gateway | 被拒绝时不创建 client call/runner call |
| 自身设置入口 | Tauri 设置命令复用 native admin surface | UI 与模型 capability 使用同一 typed 写路径；native façade 为临时迁移边界 |

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
| cancellation | gate 后、实际文件/进程/网络提交前取消 | 不调用下游，或下游按 token 停止；不以取消作为授权 |
| race/TOCTOU | check 后替换路径 component 为 reparse point | 执行入口重新 check；发现不一致即拒绝并记录净化错误 |
| 输出泄漏 | denial、tool error、日志 tail、MCP env | 不返回 key/token、完整命令输出或原始 provider/MCP secret |

以上矩阵由 `haven-tools` 的 SafetyGateway 单元测试覆盖核心判定；文件系统
重解析点测试在 Unix 使用 symlink fixture，Windows 使用同一实现的
`FILE_ATTRIBUTE_REPARSE_POINT` 检测路径编译验证。真实用户目录、注册表、网络、
电源和桌面输入不属于自动化 fixture 的目标。

