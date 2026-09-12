# 模型体验优化清单

> 目标：减少模型在每一步花在“理解工具菜单、确认能力、找回截断字段和判断是否能重试”上的预算，把上下文留给任务推理。
>
> 范围：本清单基于一次真实 Haven 会话中的 runtime snapshot、`tools[]`、`files` 续读、媒体能力和失败恢复行为整理，不是泛化的 Agent 架构建议。

## 结论

Haven 已经具备可恢复 PC agent 的骨架：runtime snapshot 固定环境，`files.outline` 与游标支持大文件续读，静态提示与动态 SESSION CONTEXT 分层，后台任务自动唤醒，工具结果有统一 observation 上限。本轮已补齐 P1 决策摩擦：

1. 能力状态必须描述“现在能否执行”，而不只是某个模型角色是否配置；
2. 观测截断必须保留错误、路径、hint 和续读游标；
3. 仓库会话的相对路径应默认对齐工作区，同时保留 Temp 沙箱；
4. 只读操作和可能造成副作用的操作必须分开处理重试；
5. `tools[]` 和 runtime snapshot 必须使用同一份能力判断。

P1 还增加了独立的 operation view、文档页游标、原生视频 ContentPart、非阻断偏好/清单和可行动的 memory 空结果诊断；聚合工具仍保留给 native/Tauri 调用和低频能力。

## P0：本轮已落地

### P0.1 能力裁剪与真实能力快照

- `media.describe` / `media.transcribe` 按实际路由角色和 provider capability profile 裁剪；不再因为 `router` 存在就默认可用。
- `audio.record` 只有在共享录音管线确实存在时才进入 schema；录音本身可先生成受管音频资产，转写能力另由 `stt` 状态表达。
- 录音先保存 WAV 并返回 `asset_id`；STT 失败时仍保留该资产，后续可调用 `media.transcribe`。
- `audio.record`、`media.transcribe` 以及 `files.read_text` / `files.summary` 的富媒体转发共享同一专用 STT 客户端；只有未配置专用客户端时才回退到 LLM STT 路径。
- runtime snapshot 新增 `runtime_capabilities`，明确输出 `web_search`、`vision`、`stt`、`audio_recording` 和 `tts` 的实际状态。
- 无 provider 内置搜索且没有可识别 MCP 搜索服务时，直接说明：`web_search: unavailable (no provider builtin search; no MCP search server)`。

### P0.2 structured-first observation

当结果是 JSON object 时，先按恢复价值排序保留 `error`、`outcome`、`path`、`root`、`next_offset`、`next_start_line`、`hint`、`asset_id`、`action_id`、`status` 等字段，再压缩正文或大数组。失败结果也会把错误和输出元数据放在同一个 observation 中。

这不改变持久化结果，只改变统一的模型可见 observation；`summary_text()` 仍保留原有的纯文本语义。

### P0.3 仓库会话默认路径

- `shell` 和 `files` 的相对路径在当前进程位于仓库时解析到检测到的 workspace root。
- 显式绝对路径、受管 `asset_id` 和安全沙箱规则不被改写。
- 没有仓库标记时继续使用原有的 Temp fallback；runtime snapshot 同时给出 `tool_default_cwd` 和 `tool_sandbox_cwd`。
- Windows PowerShell 5.1 的明显 `&&` / `||` 链式语法会在启动子进程前返回结构化错误，并提示使用 `;`、`cmd` 或 `pwsh`。

### P0.4 只读重试分离

- `files.read_text`、`files.outline`、`files.summary`、`files.search` 及 native `files.list` 标记为可安全重试；写入、编辑、复制、移动、删除标记为不可安全重试。
- `http` 原有的 GET/POST 判定继续作为权威。
- `ToolDef.json()` 增加静态 `retry_safety` 字段；具体调用失败时 observation 增加实际的 `retry_safety`。它是重放提示，不是权限放行。
- executor 仍只对幂等且满足 transient 条件的调用自动重试；未知超时和副作用操作不会被自动重放。

## P0 验收标准

- schema 中不存在当前未配置的 `audio.record`、`audio.speak`、`media.describe` 或 `media.transcribe` 分支；能力热更新后 catalog 与 snapshot 一起刷新。
- 在 observation 上限内，`files.read_text` 的 `next_offset` / `next_start_line`、路径和 hint 不被正文吞掉；失败 summary 同时保留错误与路径。
- 在仓库子目录执行省略 `cwd` 的 shell 命令，工作目录为 workspace root；没有仓库时仍回退到 Temp。
- 只读失败不会因为 `idempotency=unknown` 被迫升级为用户确认；非幂等和未知终止仍不可自动重试。
- provider-facing 参数仍经过原有 schema 投影，公共聚合工具名称和持久化 ID 契约不变。

## P1：本轮已完成

### P1.1 operation 级 schema 视图

公共聚合工具继续作为 native/Tauri 入口；模型目录同时注册高频读路径的独立瘦视图：`system.info`、`files.read_text`、`files.outline`、`files.summary` 和 `files.search`。每个 view 固定 operation/scope 并复用同一个执行实现、权限、取消、重试和 session 注册逻辑。

provider tool name、权限矩阵、session catalog 和恢复路径已按独立名称接入；未知 operation 不会通过任务意图猜测。

### P1.2 搜索与 outline 的模型视图

- `files.search` 命中项增加稳定的 `match_reason`、`context.before/after`，结果保留 `root`、`pattern`、`has_more`。
- `files.outline` 给出 `range`、`symbol_count`、`has_more` 和 `next_page.start_line`，避免模型先读一段再猜文件结构。
- `media(operation="extract")` 对 PDF/Office 文档返回 `page_index`、`total_pages`、`next_page`，不会要求模型从聚合文本自行分页。
- observation 截断统一采用 structured-first，不再让每个工具自行发明裁剪格式。

### P1.3 非阻断偏好收集

保留 `ask` 作为高代价决策的暂停确认；`preferences` / `checklist` 已作为按 session 隔离的不暂停、低风险机制，用于记录风格、详细程度、是否并行等偏好和待办项。它们不承担权限确认或副作用授权。

### P1.4 Memory 空结果诊断

`empty_reason=no_hits` 与 `diagnostics` 已同时返回 keyword/vector 来源状态和建议动作（扩大 query、尝试另一 kind、移除 subject filter、配置 embeddings）。空结果仍是成功的只读结果，不伪装成系统错误。

## P1 验收标准

- 模型 tool catalog 同时提供独立 operation view 与聚合入口；view 的 schema 不包含无关写操作字段。
- 搜索/outline/文档抽取的返回值包含可继续使用的结构化游标和范围信息；`next_page` 不依赖正文切分。
- inline 视频在具备视频能力的 provider（当前 Gemini）走原生 `ContentPart::Video`/`inline_data`；不支持的 provider 只允许显式文本占位降级，不静默转成图片或丢弃。托管视频上传和 keyframe 抽取仍需各 provider 的专用能力评审。
- `preferences` / `checklist` 不产生 ask/confirm 暂停，且 session 之间互不泄漏。
- memory 空结果包含来源诊断和建议动作，同时保持 `success=true`。

## P2：体验打磨

- 短工具索引改为“何时用 / 何时不用 / 关键 operation”三行，继续把 `tools[]` 作为 schema 权威。
- 截图、附件和生成媒体结果把 `asset_id` 放在 observation 最前，并在 notes 明确“优先读取上一条 tool result 的 `asset_id`”。
- 原始附件的请求投影附带 `media_plan: asset_id → representation` 短标记，说明该表示已经随请求发送；需要另一种表示时直接调用 `media(asset_id=...)`。
- `shell` schema 顶部继续保留当前 default shell 的两条硬规则，并对更多明显 bash/cmd 语法提供预校验。
- 能力状态继续扩展为 `web_search`、`vision/image`、`tts`、`stt` 的可用性，而不是模型名称或配置存在性。

## 不变量与安全边界

- `workspace_root` 只改善相对路径的默认定位，不绕过 `AuthorizationEngine`、allowed paths、reparse-point 和 managed asset 校验。
- `retry_safety` 不等于 `risk_level`：可重试的读操作仍可能需要权限；不可重试的写操作仍不能因为失败而自动重放。
- 结构化 observation 是模型视图，不改变 X12 `ReActSnapshot.events` 权威、不新增数据库迁移。
- provider/model 的 capability profile 是协议能力与运行时路由的交集；未知能力不写成可用。

## 验证入口

最小验证：

```powershell
cargo fmt --all -- --check
cargo test --locked -p haven-common
cargo test --locked -p haven-tools
cargo test --locked -p haven-llm
```

完整门禁仍按 `AGENTS.md` 和 `docs/development-standards.md` 执行：workspace test、clippy、UI check/test/build，以及跨 crate 契约检查。

## 回滚

本清单对应的实现是代码、prompt 和工具 JSON 的加性变更，不改变数据库 schema。可以按 ADR 0128 回退；若只回退某一项，需同步回退 runtime snapshot 文案、`ToolDef.json()` 字段和对应正/负测试。
