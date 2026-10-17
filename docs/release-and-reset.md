# 发布与数据重置

配置运行时 apply 失败时，已通过同目录临时文件和路径替换写入的配置会保留，Settings 会说明部分运行时未应用；用户重启应用后，运行时从配置重新初始化。保存成功不承诺操作系统崩溃或突然断电后的稳定介质持久性。此 apply-failure 语义本身不更改 `config.toml` 格式或数据库 schema；本版本授权变更的 schema 重置要求见下文。

## 当前版本的兼容性政策

Haven 处于测试阶段。数据库 schema、`config.toml` 与内部 IPC 契约可以进行破坏性调整；发布说明会明确本次是否需要重置。没有明确写出兼容承诺的旧数据不得假定可继续使用。

截至 2026-10-17，当前数据库契约为 schema v45（v39 的持久数据与交互契约见 ADR 0392、0393、0402、0416、0442、0445、0458、0463、0464、0524、0633、0809）：后台与定时工具运行统一持久化在 `tool_runs`，`kind` 仅接受 `background` 与 `scheduled`，依赖关系使用 `watch_tool_run_id` / `result_summary`，定时运行完成结果使用 `tool_run_completion_outbox`；session authorization grants 由会话外键级联管理。`pending_session_inputs` 持久跟踪尚未进入 `UserInject` event 的用户输入，并在同一事务保存 `answer` / `steering` / `follow_up` disposition，不再使用两天恢复窗口。唯一的 pending Answer reservation 防止 Ask 尚未由 `UserInject` 确认时后续输入被重复路由为答案；恢复只读取该 marker，不按重启时的交互 gate 重新判断。`sessions.origin` 与 `sessions.parent_session_id` 持久记录普通用户会话、`agent.spawn` peer 或 fork 会话的来源及 parent lineage，且不改变 session lifecycle。sessions.run_end_reason 保存最近一次 paused、completed 或 error 运行的净化后原因；新一轮进入 pending/running 时清除。后台 shell ToolRun 持久化 `tool_runs.source_step_id`，关联发起它的 Agent `ToolCall`，并在 ToolRun event 与终态结果交付中保留。v37 不再读取旧 interaction event 的 `prompt` 字段或缺少 payload `step_number` 的旧 compaction event，也不做旧 schema 运行时迁移；升级前，完全退出 Haven 后删除 `%APPDATA%\haven\haven.db`、`haven.db-wal` 与 `haven.db-shm`（非 Windows 开发环境为 `~/.local/share/haven` 下的同名文件），再启动应用。旧的 `actions` 表、`act-` ID、completion outbox 与 tool name 不做兼容迁移。删除数据库会清除会话、记忆、工具运行和用量。

schema v41 仅接受事实提取 marker 的当前完整格式：旧 boolean、缺 retry metadata 的短格式和 summary value-only marker 都不再解析；`CacheDiagnostics` 也要求当前完整 metadata shape。它还统一 BranchPoint 的 projection cutoff：旧 branch point 仅记录 message watermark，无法从 append-only event 中安全补算 step 与 usage 水位（ADR 1043）。它还为 `tool_runs.kind` 增加闭合集合约束（ADR 1080）。v40 数据库不含该约束，升级前必须按下文删除数据库。`MediaProbe` 使用 `media_kind` / `mime_type` Serde key，不接受旧 `media_type` key；它不属于持久表或当前 IPC DTO。MCP `inputSchema` 和 LLM provider wire 字段仍按当前外部协议解析，不属于 Haven 历史数据兼容。
schema v42 为 `partial_messages` 增加 `ingress_seq` checkpoint 基线，并在同一事务中验证 checkpoint 未被后续消息覆盖、写入 canonical message 与删除 checkpoint；写入失败会保留 checkpoint，启动时继续重试（ADR 1143）。v41 及更早数据库不做运行时迁移，升级前需按下文删除数据库文件。

schema v43 为待投递输入增加 `steering` 路由，恢复时保留「立即调整」与「完成后追加」意图（ADR 1156）。v42 及更早数据库不做运行时迁移，升级前需按下文删除数据库文件；配置与 inbox 无需重置。

schema v44 为会话来源增加 `fork` 与 `fork_source_message_id`，支持从用户消息派生独立会话；派生事务会复制有效 transcript 前缀及对应 messages/steps，并重映射 child-owned `msg-*` / `step-*` 身份（ADR 1157）。v43 及更早数据库不做运行时迁移，升级前需按下文删除数据库文件；配置与 inbox 无需重置。

schema v45 收敛 durable session event DTO：interaction/transcript/usage 身份只接受规范字段，ToolResult envelope 使用 `idempotency`，不再为旧字段名或旧 JSON shape 提供读取 fallback（ADR 1296）。v44 及更早数据库不做运行时迁移，必须先备份并按下文删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`；升级会清除会话、transcript、记忆、授权、ToolRun 和用量。

跨 session 消息 inbox 只接受规范 `ses-{uuid32}` 身份、规范 `msg-{uuid32}` envelope id 和当前必填字段 shape。旧 registry、缺 `Envelope.type` 的 JSONL、缺 `AgentEntry.capabilities` 的条目、旧 backlog version marker 与超过当前 serialized envelope 上限的行均不迁移；坏行阻塞时 source 保持不变。切换格式前必须先完整备份并按下方步骤重置整个 `inbox/`；这会清除所有 peer 的 registry、未处理消息、backlog/cursor、processing、archive 与 deletion tombstone，但不删除 SQLite transcript、会话或记忆。每 recipient 新写入预算为 16 MiB，history 最多返回 100 条；删除谱系 `deleted_sessions.json` 最多 64 MiB。升级期间必须退出所有共享该目录的 Haven 进程，不能让旧 binary 与新格式并发访问。备份保留原始行供离线检查，不能直接作为运行时数据恢复。

## 当前配置契约

`config.toml` 文件上限为 4 MiB。超过上限的文件在启动读取或外部修改指纹检查时直接报错：原文件不备份、不改写，也不会回退到默认配置；Settings 序列化结果超限会拒绝更新并保留已有文件、快照和版本。请缩小 `config.toml` 后重启，无需删除数据库。

媒体 provider timeout 的范围与 Settings UI 共用同一契约：`[media.stt].timeout_secs` 为 5–600 秒，`[media.ocr].timeout_secs` 与 `[media.tts].timeout_secs` 为 5–300 秒，`[media.image_gen].timeout_secs` 为 10–600 秒。超出范围的 TOML 会备份整份配置并在当前进程使用默认值；Settings 更新则拒绝写入并保留现有配置。需要保留其它配置时，先把这些值改到范围内；无需删除数据库。

录音时长与静音停止范围由 Common 配置校验和 Settings UI 共同执行：`[media.audio].max_duration_secs` 为 10–300 秒，`silence_timeout_ms` 为 500–10,000 毫秒。`sample_rate`、`channels`、`bits_per_sample` 已移除，因为采集输入格式由设备运行时选择，转写 WAV 固定为 16 kHz、单声道、16-bit PCM。旧 `config.toml` 若含这三个键会被拒绝并备份；删除这些行即可保留其余设置。无需删除数据库。

技能脚本执行配置已移除不生效的 `[skills_exec].cpu_time_secs` 与 `max_memory_mb`。旧 `config.toml` 若含任一键会因未知字段被拒绝并备份，当前进程使用默认配置；删除这两行即可保留其余设置。`[skills_exec].work_dir` 现必须是绝对本地路径；若旧值为相对路径，需将它改为绝对路径或仅重建 `config.toml`。无需删除数据库。

并发范围由 Common 配置校验和 Settings UI 共同执行：`[session].max_concurrent` 为 1–10，`[llm].max_concurrent_requests` 为 1–16，`[context_limits].max_running_background_tool_runs` 为 0–64。后台任务上限设为 0 会禁用新的后台任务。超出范围的 TOML 会备份整份配置并在当前进程使用默认值；Settings 更新会拒绝该值。需要保留其它配置时，先把数值改到范围内；无需删除数据库。

定时触发时间受 `context_limits.scheduled_tool_runs_due_horizon_secs` 限制，范围为 0 至 10 年；0 禁止未来的 `due_at` 和 `delay_secs` 时间触发。后台任务直接启动和等待另一 ToolRun 完成的 `watch_tool_run_id` 依赖触发不使用时间 horizon。Settings 与运行时使用相同边界；配置越界会备份并回退默认值，无需重置数据库。

资源配置边界现由 Common 与 Settings UI 共同执行：`[session].prompt_history_limit` 为 0–500，`[session].history_retention_days` 为 0–365（0 禁用自动清理）；`file_max_list_entries` 最多 10,000；MCP SSE 缓冲最多 4 MiB；单个 `SKILL.md` 最多 1 MiB；网络响应体最多 16 MiB。以上容量设为 0 时分别表示不带历史、禁用会话自动清理、返回空目录列表、丢弃 SSE 事件、跳过所有技能说明、不保留响应体。超过上限的 TOML 会备份整份配置并在当前进程使用默认值；Settings 更新会拒绝越界值。把数值改到范围内即可保留其它配置；数据库不受影响。

模型上下文窗口（`context_limits.default_context_window` 与模型级 `context_window`）为 1,000–2,000,000 tokens；压缩预留 token 必须小于每个有效窗口，`response_token_floor` 最多 2,000,000。`tool_settings.*.max_retries` 最多 10 次，重试退避最多 60 秒，单工具 timeout 为 1–3,600 秒。剪贴板历史保留和单次查询各最多 100 条；保留数为 0 会清空并停止记录，查询上限为 0 返回空列表。进程内每条保留文本最多 65,536 个 Unicode 字符，`clipboard_entry_max_chars` 只限制 history 输出。TOML 超界按既有策略回退默认值，Settings 更新拒绝，无需重置数据库。

`config.toml` 只接受当前配置结构，不执行旧字段搬迁、旧名称映射、凭据导入或静默兼容。配置表启用未知字段拒绝；当前结构允许缺省的字段仍使用安全默认值。本版本将 `context_limits.cut_off_retries` 改为 `incomplete_tool_args_retries`，将后台/终态 Job 限制字段重命名为 ToolRun 字段，并删除 `empty_response_max_retries` 与 `empty_response_retry_delay_ms`；含这些旧字段的配置会导致整份配置解析失败。工具根名从 `actions` 改为 `tool_runs`，旧名称下的权限 key 不迁移。`llm.models[]` 中绑定连接的字段现为 `provider_name`；`[media.stt]`、`[media.tts]` 与 `[media.image_gen]` 的连接引用也统一使用 `provider_name`，旧媒体字段 `provider` 不再接受。若只需保留现有设置，可将这些媒体表中的 `provider` 手动改为 `provider_name`；否则仅重建 `config.toml`。这些配置变更无需删除数据库。Session 步数预算配置现为 `[session].max_steps_per_run` 与 `[session].max_steps_per_session`；旧 `max_steps` / `session_max_steps` 不接受，遇到旧 key 时整份配置解析失败；如需保留预算数值，先手动将两项改为新 key，否则重建配置。其它旧字段（例如 `[memory].history_retention_days`、`llm.balanced_model`、旧安全策略字段和已删除的顶层 `[audio]`）也会导致整份配置解析失败。

`context_limits.max_response_tokens` 实际是 endpoint `max_tokens` 的下限，现名为 `response_token_floor`；含旧 key 的配置会整体解析失败。需要保留数值时，手动将 key 改为 `response_token_floor`，或仅重建 `config.toml`；数据库不受影响。

文件工具限额现以字节命名：`file_read_max_chars`、`file_max_line_chars`、`file_max_byte_read` 分别改为 `file_max_full_file_bytes`、`file_line_max_bytes`、`file_byte_read_max_bytes`；整文件载入上限与 byte-mode/inspect/hash 读取上限各自独立，写入使用新增的 `file_write_max_bytes`（默认 16 MiB）。旧 key 不接受；需保留原数值时手动改名，并按需设置独立写入上限，否则仅重建 `config.toml`。数据库不受影响。

MCP 二进制限额 `mcp_max_binary_payload_bytes` 改为 `mcp_max_binary_base64_bytes`，名称现在说明上限计量的是保留在观察中的 Base64 UTF-8 字节，而非解码后的媒体大小。旧 key 不接受；需保留配置时手动改名。数据库不受影响。

SKILL.md 单行解析上限 `skills_max_line_len` 改为 `skills_max_line_bytes`，上限一直按 UTF-8 字节计量，UI 和解析错误现在也明确显示字节。旧 key 不接受；需保留配置时手动改名。数据库不受影响。

ToolRun 数量上限按实际计数状态重命名：`scheduled_tool_runs_max` 改为 `max_pending_scheduled_tool_runs`（仅计入尚未触发的定时任务），`background_max_tool_runs` 改为 `max_running_background_tool_runs`（仅计入运行中的后台任务）。默认值和计数行为不变，旧 key 不接受；需保留配置时手动改名。数据库不受影响。

运行中的 `ConfigService` 会在持久化前核对 `config.toml` 是否仍与加载时一致；若 Haven 外部编辑了配置，当前进程会拒绝覆盖并提示重启加载最新配置。外部修改无需删除配置或数据库。

Session prompt 首次上下文条数目前配置在 `[session].prompt_history_limit`（默认 50）；旧 `[memory].session_window_size` 不再接受或自动搬迁。若要保留其它配置，手动将数值移到新 key 并删除旧 key；否则按“仅重建配置”删除 `config.toml` 后重新设置。遇到旧 key 时配置会整体解析失败、先备份原文件并在当前进程使用默认值；数据库不受影响。

配置解析或语义校验失败时，Haven 将原文件复制到带时间戳的 `config.toml.*.bak`，并在当前进程以默认值提供运行时配置；该恢复态为只读，Settings 加载和配置修改、凭据暂存都会返回恢复错误，避免把默认值写回并覆盖原文件。若自动备份失败，原文件仍保留，恢复态也仍然只读。需要继续使用时，先从备份恢复并修正原文件，或按下文“仅重建配置”删除 `config.toml`，然后重启应用；启动时重新加载有效文件或创建新默认配置后，Settings 才恢复可写。无需删除数据库。

当前会话保留期配置位于 `[session].history_retention_days`，默认 90 天，设为 `0` 可禁用自动删除；旧 `[memory]` 位置不迁移。`security.permission_mode` 支持 `plan`、`default`、`auto_edit`、`autonomous`，文件沙箱和网络策略分别使用 `sandbox_mode` 与 `network_policy`；旧确认模式字段不再解释。权限 key 使用点号 operation 名（如 `files.read`）；旧冒号 key 不转换，无法解析的持久权限规则在运行时被忽略并 fail-closed。

API 密钥、OCR 密钥与 MCP 环境变量只通过安全凭据存储的 opaque reference 持久化。TOML 中的明文凭据会使配置加载失败并备份原文件；启动只从现有引用读取凭据，不再导入旧明文值。若引用在操作系统凭据存储中不存在，需要重新输入对应凭据。当前 provider `api_style` 仅接受 canonical wire protocol id；无效值和指向不存在 provider 的媒体配置会走同一备份与默认配置恢复。Deepgram 凭据只填写原始 API key，不要包含 `Token ` 或 `Deepgram ` 前缀；已保存前缀的凭据需要在模型设置中重新输入。

Provider 的 `media_capabilities` 可选字段列出该连接实际提供的 STT、TTS 和图片生成 endpoint。旧配置没有该字段时，官方 OpenAI、Gemini、Groq、Deepgram、AssemblyAI、ElevenLabs 按已知 profile 推导能力，其它兼容服务默认关闭；这不影响聊天模型路由。设置页可在对应 wire style 支持范围内显式启用媒体 endpoint。该字段为可选配置，没有数据库迁移或重置要求。若需回退到不认识该字段的旧版本，先从 `config.toml` 的 Provider 表中删除 `media_capabilities`。

模型工具使用点号 operation view，例如 `files.*`、`system.*`、`clipboard.*`、`input.*`、`window.*`、`media.*`、`tool_runs.*`、`schedule.*` 和 `haven.*`。启用 Skill 由 `load_skill` 按名称加载为当前 session 的 `skill__...`；内置 operation 由 `tool_catalog` 的 `action=load` 加载，MCP 由 `load_mcp` 按服务器加载。配置和未完成会话都没有旧工具名的转换保证。

媒体与搜索设置保持运行时定义的值域：`[media.ocr].provider` 仅接受 `llm`、`baidu`、`azure`、`tencent`、`none`；STT 的 `provider_name=mcp` 必须引用已配置的 `[mcp_servers]` 名称；TTS/image generation 的 provider 名称可用空值或大小写不敏感的 `none` 禁用。LLM provider/model 的 `default_web_search` / `web_search` 接受 `off`、`auto`、`always` 及 `required`、`on`、`1`、`true` 别名（大小写不敏感并忽略外围空格）；`null` 或空白继续回退到 `HAVEN_WEB_SEARCH`，显式 `off` 才关闭。未知 OCR/search 值和悬空 STT MCP 引用会使整份 TOML 备份并在当前进程使用默认值；Settings 更新拒绝错误字段。修正对应配置即可保留其它设置，无需重置数据库。

Skill 名称现在统一限制为 1–128 个 ASCII 字母、数字、`-` 或 `_`，并拒绝 Windows 设备保留名（`CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9`，不区分大小写）。扫描时，同名大小写变体会作为歧义组全部跳过。已有 `SKILL.md` 使用不符合规则的名称时，Skill 会被跳过；将清单中的名称改为有效名称即可恢复使用。若配置了 `[skills].enabled` allowlist，也要同步更新对应条目。此名称规则不会改变数据库契约，改名无需重置数据库。

文件、搜索与模型工具资源边界由 Common、Settings UI 与运行时共同执行：`file_line_max_bytes` 为 1–128,000 字节，`file_byte_read_max_bytes` 为 1 字节–64 MiB，`search_max_results` 为 1–1,000，`search_window_bytes` 最多 16 MiB，`embedding_chunk_size` 为 1–10，`max_tools_per_request` 为 1–350。行长度和 byte-read 上限不能设为 0；配置 0 会备份并回退默认值，Settings 更新会拒绝。每次文件搜索的 `results` 数组最多物化 4 MiB 序列化字节，根路径最多 32,767 个 UTF-16 code unit，搜索模式最多 4,096 个 Unicode 字符；路径和模式另有独立长度上限，触及结果预算时返回明确截断提示。行范围窗口为 0 时仍改用流式全文件扫描。调整数值即可保留其它配置，无需重置数据库。

`context_limits.input_ring_buffer_secs` 范围为 0–120 秒；设为 0 使用采集引擎默认环形缓冲容量（20 秒），更改仅在下次创建采集引擎时生效，当前运行中的引擎保持现有容量。`max_known_facts` 为 0–64；超范围的 Settings 响应会被拒绝，配置文件按现有策略备份并回退默认值。无需重置数据库。

## 用户数据位置

Windows 的唯一数据根目录是 `%APPDATA%\haven`。其中包括：

- `config.toml`：模型、工具与安全确认配置；
- `haven.db`（及 SQLite journal/WAL）：会话、记忆与任务；
- `logs/`、`media/`、`skills/` 与 `inbox/`：日志、生成媒体、本地技能及多会话消息。

非 Windows 开发环境使用 `~/.local/share/haven`。路径由 `haven_common::config::ConfigLoader::data_dir()` 统一决定，宿主层不得另行推导。

## 重置步骤

### 仅重建配置

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. 如需检查旧配置，先在 `%APPDATA%\haven` 之外复制 `config.toml`；旧文件或自动备份可能包含明文密钥，必须妥善保管。
3. 删除 `%APPDATA%\haven\config.toml`（非 Windows 开发环境为 `~/.local/share/haven/config.toml`）。
4. 重新启动 Haven，再配置模型、OCR 与 MCP 凭据。schema v45 版本升级还必须按下方说明重置数据库。

### 仅重置消息收件箱

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. **切换到当前 Messaging 持久格式前，必须先在 `%APPDATA%\haven` 之外完整备份 `inbox/`，然后删除整个目录。** 该版本不迁移或重放旧 inbox 内容；重置会清空所有 peer 的 mailbox、processing、backlog 与 cursor、registry、archive 和 deletion tombstone。内容可能包含会话消息，备份应妥善保管。
3. 删除 `%APPDATA%\haven\inbox`（非 Windows 开发环境为 `~/.local/share/haven/inbox`）。不要只删某个 `.jsonl`、`.processing`、`.backlog`、`.backlog.state.json` 或 sidecar 文件。
4. 重新启动 Haven；Messaging 会按规范 session ID 重建 registry 和 mailbox。SQLite transcript、会话与记忆不受影响。

`.lock` 是持久的 OS 文件锁载体，正常运行时文件会保留；不要单独删除。若有进程仍持有该文件，删除整个 inbox 后启动的新进程可能锁住新的文件对象，因此执行重置前必须退出所有共享该目录的 Haven 进程。

Messaging 只接受当前规范 envelope、registry 与 backlog cursor 形状，并以 envelope 序列化上限约束每一条 JSONL 记录。旧格式及超限行不会导入、迁移或在线跳过；source 文件会在读失败时保留。没有针对单条坏行的在线跳过或 quarantine 工具。升级切换必须按上方步骤完整备份并重置整个 inbox；这会影响所有 peer 并清除 deletion tombstone。执行前应确认所有旧 Haven 进程均已退出。若要保留旧消息状态，应停留在兼容该 inbox 的旧版本，等待专门恢复工具，不要手工改写 backlog/cursor sidecar。

### 完整重置数据根目录

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. 如需保留配置或诊断资料，先在数据根目录之外复制所需文件；备份内容可能含密钥、对话和本机路径，必须妥善保管。
3. 删除 `%APPDATA%\haven`（非 Windows 为 `~/.local/share/haven`）。
4. 重新启动 Haven；应用会创建新的默认配置和数据库。

完整重置会永久删除本机会话、记忆、后台/定时 ToolRun、授权决定、日志、技能和媒体缓存；除非先自行备份，否则无法恢复。schema v45 升级要求删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`，否则旧数据库会被拒绝打开。只有配置仍含不再接受的旧 key（如 `llm.models[].provider`）时，才另外按“仅重建配置”删除 `config.toml`。

## 发布前验证

发布候选版本必须在全新数据根目录完成：启动、默认配置创建、模型配置、会话、工具确认、媒体和任务流程、重启恢复、升级重置和卸载验证。执行的自动化质量门禁见仓库根目录 [README](../README.md)；它们不能替代真实桌面安装流程。

自动化测试必须使用专用、运行前确认不存在的数据根目录。Rust workspace 测试应把 `APPDATA` 指向仓库 `target` 下唯一的审计/测试目录，避免默认 `%APPDATA%\haven`；会启动清理任务的 AppState 测试必须注入 fixture 自己的上传和生成媒体根目录。不要把测试 `APPDATA` 设置为日常使用的配置路径，也不要用现有用户目录验证重置或卸载。

首次启动、安装升级、数据库重置和卸载需在一次性 Windows 用户配置或 VM 中验证，并在操作前确认目标路径属于该临时环境。仓库当前的单元/集成测试不执行这些破坏性桌面步骤。2026-09-26 最终架构审计的隔离方式、通过门禁和未执行项见 [ADR 0361](adr/0361-final-architecture-acceptance-audit.md)。

## 回滚

仅在保留了升级前的完整数据根目录备份，并且旧二进制与该数据版本兼容时，才可通过恢复备份回滚。没有兼容性保证时，回滚方案是安装目标版本后按上述步骤重置数据，而不是混用新旧数据库或快照。
