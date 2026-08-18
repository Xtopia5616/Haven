# Haven 命名规范

> 版本: v1.0 | 日期: 2026-08-17

本文档统一 Haven 项目各层的命名规则（变量名、函数名、文件名、crate 名、缩写大小写、跨层边界）。规范以现有代码中的事实模式为基础，新代码必须遵循；存量代码若与规范冲突，逐步迁移对齐。

## 总则

- **分层语气不同**：Rust 后端与 Svelte 前端各自遵循本语言生态的惯例，二者仅在跨层边界（Tauri 命令 / 事件 / 数据字段）约定转换规则。
- **一眼可辨**：命名应能区分「类型」「值」「常量」「组件」「模块」，见各层细则。
- **先查后设**：新增命名前先查是否已有同义词，避免重复词汇（如 `stt` 与 `asr` 语义不同，各归其位）。

---

## 1. 后端 Rust

### 文件名 / 模块名
- **snake_case**，如 `stt.rs`、`openai_responses.rs`、`scheduled_action.rs`。
- 目录即模块：`crates/tools/src/builtin/`、`crates/memory/src/repositories/`。
- crate 统一 `haven-{name}`：`haven-agent`、`haven-common`、`haven-memory`、`haven-tools`、`haven-llm`、`haven-input`、`haven-mcp`、`haven-skills`。

### 标识符
- 类型 / 枚举 / trait / 结构体 → **PascalCase**：`Modality`、`Intent`、`LlmConfig`。
- 函数 / 方法 / 变量 / 字段 / 模块 → **snake_case**：`fn detect_intent`、`stt_default_base_url`。
- 常量 / 静态 → **UPPER_SNAKE_CASE**：`SPEECH_GEN_KEYWORDS`、`IMAGE_GEN_KEYWORDS`。
- 构造 `pub const fn as_str` / `new` 保持惯例命名。

### 缩写大小写规则
- **类型名**中缩写用 PascalCase：`SttProvider`、`OcrEngine`、`TtsProvider`。
- **函数 / 文件 / 变量 / 字段**中缩写当作整词用小写：`stt.rs`、`tts.rs`、`ocr.rs`、`stt_default_base_url`。
- 一个概念只用一个缩写词，禁止换用：语音转文本统一 `stt`，OCR 统一 `ocr`，文本转语音统一 `tts`。
  - 例外：`asr` 是用户输入的关键词（意图识别 vocabulary，与 `ocr` 相邻），属于**输入信号**，不是 provider 模块名，不并入 `stt` 词汇表。二者语义不同，各归其位。

### ID 规范
实体 ID 统一 `{prefix}-{uuid32}`，一律用 `haven_common::types::new_id(prefix)`，禁止手拼。完整前缀表见 `AGENTS.md`.

---

## 2. 前端 Svelte 5

### 组件（`.svelte`）
- 文件名 = 组件名，**PascalCase**：`ApiKeyDialog.svelte`、`ToolResultCard.svelte`。
- 由 `.svelte` 文件隐式定义组件，不额外命名导出，避免名不符文件。

### 模块（`.js`）
- 工具 / 状态模块 → **camelCase**：`streaming.js`、`voiceSubmit.js`、`markdownRenderer.js`、`sessionStatus.js`、`modelRoles.js`。
- 主要导出 Svelte store 的模块 → `xxxStore.js`：`themeStore.js`、`syncStore.js`（`syncStore.js` 导出同名的 `syncStore` 辅助函数，名随主导出）。
- 聚合 store 桶文件保留 `stores.js` 命名（导出 `sessionStore`/`actionStore` 等命名导出）。
- 常量 → **UPPER_SNAKE_CASE**：`ACTION_STATUSES`、`COLOR_MAP`、`ROLE_KEYS`。
- 局部变量 / 函数参数 → **camelCase**：`newKeyValue`、`reasoningOpen`、`ctxMenuItems`。

### 路由
遵循 SvelteKit 约定：`+page.svelte`、`+layout.svelte`，目录用小写连字符（`dev-recording/`、`settings/`、`tools/`）。

---

## 3. 跨层边界（Rust ↔ 前端）

| 域 | 后端 | 前端 |
|---|---|---|
| 标识符 | snake_case | camelCase |
| 事件字段 | `session_id`、`step_number` | `sessionId`、`stepNumber` |
| Tauri 命令 | snake_case（`get_log_info`） | invoke 时转换 |
| 实体 ID | `{prefix}-{uuid32}`（统一） | 原样透传 |

- **前端只在边界转换**（invoke 调用 / 事件监听处），内部统一 camelCase。
- **Reactive / 数据字段**（如 DB 行字段、测试 fixture）允许保留后端 snake_case，不强行改前端内部就 camelCase 化。
- 不要在调用链深处出现重复的手工 snake↔camel 转换；将来集中收敛到命令/事件封装层。

---

## 4. 名词单复数

- **容器 / 集合 / 表 / 目录 / 仓库** → **复数名词**：`sessions`、`messages`、`actions`、`facts`、`session_steps`、`memory_embeddings`、`modelCards`、`messages`。
- **单一实体 / 单行元素** → **单数**：`session`、`message`、`action`、`row`、`card`、`msg`。
- **不可数 / 质量名词** 保持单数：`usage`、`audio`、`video`、`text`、`schema`、`kv_store`（复合词不数）。
- **前端 store 变量** 按承载实体命名（`sessionStore`/`actionStore` 可承载数组/对象，名字取实体单数，属约定）。
- **派生集合结果** 用「实体＋复数」或复数词，避免用裸形容词承载集合：写 `selectedSessions`、`filteredMessages`、`remainingMessages`、`keptExistingMessages`，不写 `selected`/`filtered`/`remaining`/`keptExisting` 指代数组。
- store `update` / `filter` / `map` 的回调单元素参数用单数短名（`m`/`x`/`row`/`card`/`t`），保持单数语义。
- **文件名 / 结构体名保持一致**：一个文件一个实体时文件名单数；实体本身为集合资源（`Files`/`Actions`/`Facts`）时文件名随结构体用复数：`files.rs` ↔ `FilesTool`、`actions.rs` ↔ `ActionsTool`、`facts.rs` ↔ `FactsTool`（启动名 `"files"`/`"actions"`/`"facts"`）。
- 仓库 / 表名按所管理实体的复数命名，与其承载集合一致：`sessions.rs`、`messages.rs`、`facts.rs`、`session_steps`。
- 不可数名词文件（`usage.rs`、`audio.rs`、`text.rs`、`schema.rs`）保持单数。

> 该漂移已对齐：`scheduled_action.rs`↔`ScheduledActionTool`、`env.rs`↔`EnvTool`（原 `env_var.rs`）、`system.rs`↔`SystemTool`（原 `SystemInfoTool`）。新代码避免再制造 `Xxx` 与文件名不同词的情况。

---

## 5. 命名自查清单（提交前）

- [ ] Rust 文件 / 模块 snake_case，类型 PascalCase，常量 UPPER_SNAKE
- [ ] 缩写整词统一（`stt`/`ocr`/`tts`），不混用别名
- [ ] 实体 ID 用 `{prefix}-{uuid32}`，经 `new_id` 生成
- [ ] Svelte 组件文件名 = 组件名（PascalCase）；JS 模块 camelCase，store 尾缀 `Store`
- [ ] JS 局部变量 camelCase，常量 UPPER_SNAKE
- [ ] 跨层只在边界转换 snake↔camel
- [ ] 集合用复数、单元素用单数、派生集合不用裸形容词（`selected`→`selectedSessions`）
- [ ] 文件名与结构体/实体单复数一致（`file.rs`→`files.rs` 对应 `FilesTool`；仓库随表复数）
