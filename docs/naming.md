# Haven 命名规范

> 版本: v1.3 | 日期: 2026-10-07

本文档统一 Haven 项目各层的命名规则（变量名、函数名、文件名、crate 名、缩写大小写、跨层边界）。规范以现有代码中的事实模式为基础，新代码必须遵循；存量代码若与规范冲突，逐步迁移对齐。

## 总则

- **分层语气不同**：Rust 后端与 Svelte 前端各自遵循本语言生态的惯例，二者仅在跨层边界（Tauri 命令 / 事件 / 数据字段）约定转换规则。
- **一眼可辨**：命名应能区分「类型」「值」「常量」「组件」「模块」，见各层细则。
- **边界语义不撞名**：不同 crate/边界中的类型即使位于不同命名空间，也要能从类型名看出领域或 wire 角色；同名但不同状态空间时用领域限定词，不要求合并状态 owner。例如 Memory durable `SessionEvent` 与 Agent process-local `SessionSupervisorEvent` 各自保留 owner。
- **同域不同形状标明角色**：同一领域中的完整 runtime state、稀疏 view 输入或 wire projection 即使字段重叠，也使用能标出约束/角色的不同类型名，不用可选字段数量来猜其含义。
- **原始值与归一分类分名**：边界 parser 为向前兼容而保留的开放字符串，使用带契约/领域前缀的类型名（如 `ToolManifestSource`）；UI 内部归一到已知集合的类别保留闭合类型（如 `ToolSource`），不要让相同类型名同时表示不同约束。
- **跨边界多值结果具名**：若多个返回值各有稳定领域含义并跨模块或 crate 传递，使用具名结构体字段，不用位置元组让调用者记住各索引的语义。
- **先查后设**：新增命名前先查是否已有同义词，避免重复词汇（如 `stt` 与 `asr` 语义不同，各归其位）。

## 产品与领域术语

以下是跨 Rust、Tauri 事件、Svelte 和用户文案的统一口径。代码、wire、数据库和配置统一使用 ToolCall/ToolRun 概念；历史 ADR 中的旧名称只用于说明当时的决策，不构成当前命名契约。

| 术语 | 含义 | UI 文案 / 代码边界 |
|---|---|---|
| 工具调用（ToolCall） | Agent/模型发起的一次工具调用；前台调用等待结果并进入当前 transcript | Agent/ReAct 使用 `ToolCall`；provider 的 `tool_call_id` 保持原格式 |
| 会话（session） | 用户与 Agent 的对话及其前台 ReAct 运行上下文 | UI 直接称“会话”；前台工具调用在会话内呈现 |
| 工具运行（ToolRun） | 脱离当前 turn 持久运行、可取消并产生生命周期事件的工具执行 | 后端/IPC/数据库使用 `tool_run`、`tool_runs`；ID 前缀为 `toolrun-` |
| 后台工具运行（background ToolRun） | 工具调用选择后台执行后启动的持久运行 | 通过 `ToolExecutionMode::Background` 启动；UI 显示“后台任务” |
| 定时工具运行（scheduled ToolRun） | 由时间或依赖触发的工具运行 | 仍由 `schedule` 工具负责设置触发条件；UI 显示“定时任务” |

定时工具运行的 `mode` 只作为行为说明：`tool` 显示“调用工具”，`continue` 显示“继续会话”。运行状态统一显示“待执行 / 运行中 / 已完成 / 失败 / 已取消”；原始枚举值只留在 wire、日志或调试详情中。

`ToolExecutionMode::Foreground/Background` 表示调用执行方式；持久 `ToolRunKind` 只有 `Background/Scheduled`。会话是对话实体，不是 `ToolRunKind`，不得把“会话”塞进任务类型映射。

## 架构角色词汇

类型后缀不是装饰词：它必须说明对象的职责。新增和重命名类型按下表选用；存量不一致项在全项目术语审计中逐域处理，不做机械批量替换。一个类型若同时符合多个角色，应先明确它真正拥有的职责，再决定保留组合名还是拆分。

| 词汇 | Haven 中的约定含义 | 不应用来表示 |
|---|---|---|
| `Port` | 某层消费的窄能力接口；由上层组合根提供适配。 | 具体实现或任意参数对象。 |
| `Adapter` | 在两个稳定边界间转换数据/调用并实现目标 port。 | 领域规则的唯一 owner。 |
| `Client` | 对外部 provider、服务或协议端点执行 I/O 的调用端。 | 本地数据目录或纯配置。 |
| `Provider` | 为调用方提供某类可替换能力/数据的来源。 | 纯转换器；此类使用 `Adapter`。 |
| `Router` | 根据显式用途/能力选择目标 provider、模型或执行路径。 | provider 线协议实现或通用请求生命周期 owner。 |
| `Resolver` | 从已知输入和 owner 中解析一个值/身份/候选；不承载授权副作用。 | 数据字段纯映射（`Mapper`）或权限裁决（`AuthorizationEngine`）。 |
| `Loader` | 按请求从已知来源装入数据/能力，并说明目标作用域及失败语义。 | 仅列举/描述目录，或绕过 owner 直接执行目标能力。 |
| `Store` | Rust 后端的具名数据读写边界；持久化写入的一致性/事务由它或它委托的唯一 owner 定义。Svelte 侧沿用生态术语，`xxxStore` 表示响应式状态容器，不表示持久化。 | Rust 后端的纯缓存或通用 SQL 连接；前后端不能因同一个后缀而假定职责相同。 |
| `Repository` | Memory crate 内按持久实体组织的实现模块；跨层接口和事务 owner 优先使用 `Store` 术语。 | 与 `Store` 并列、职责不明的第二套持久化端口。 |
| `Registry` | 按稳定 key 管理权威条目/实现，提供注册、替换与查找；必要时拥有顺序、唯一性和版本。常见例子：`ToolRegistry`、`ModelRegistry`、`SkillRegistry`。 | 只展示描述或搜索结果的目录投影。 |
| `Catalog` | 供发现、列举、描述或筛选的能力/资源目录；读取目录本身不授予执行权。 | 已激活的执行集合或执行授权决策。 |
| `Index` | 为查询建立的派生查找结构；不独立拥有源数据生命周期。 | 注册/删除业务实体的权威入口。 |
| `Snapshot` | 带明确范围/版本的不可变时点视图。 | 长期可变 owner，或含糊的“当前对象”。 |
| `Projection` | 从事件或权威实体推导出的只读视图。 | 恢复/回滚的第二真源。 |
| `Cache` | 可丢弃并可重建的性能副本；必须能指出失效条件和 owner。 | 持久权威状态。 |
| `State` | 一个明确作用域内的可变状态模型；名称前部标出 session、run、页面等作用域。 | 无范围说明的全局杂项容器。 |
| `Context` | 一次操作/请求所需的显式输入与依赖视图；生命周期短且范围可读。 | 持久状态 owner 或不受控的服务集合。 |
| `Policy` | 可审查的约束、分类或决策规则。 | 只提取/映射字段、但不决定策略的 helper。 |
| `Plan` | 已校验、尚未生效的拟执行变更或动作列表。 | 已提交的状态或副作用结果。 |
| `Service` | 一个领域能力的调用面；编排该领域规则与 store/port，不做无边界的对象汇总。 | 仅转发另一对象、却不说明其 owner 语义的总入口。 |
| `Manager` | 管理一组资源的创建、替换、重连或生命周期。 | 只有组合/转发职责的 façade。 |
| `Facade` | 对内部多个窄接口提供稳定的组合调用面；不隐含额外状态权威。 | 另一份业务状态 owner。 |
| `Coordinator` | 按明确顺序协调多个既有 owner 的转换；不得暗中复制其状态。 | 通用工具箱或第二个生命周期 owner。 |
| `Gate` | 串行化临界区的同步原语，例如共享 mutex；字段名表示锁本身。 | 同时拥有持久编辑与运行时 prepare/publish 流程的协调对象（`Coordinator`）。 |
| `Runtime` | 已应用、供执行路径使用的活跃配置/资源集合；`Prepared*` 表示尚未发布的候选。 | 一次请求的临时数据包或只读快照。 |
| `Engine` | 执行有明确输入/输出的算法、循环或判定过程。 | 负责装配所有依赖的组合根。 |
| `Worker` | 消费队列/outbox 或周期任务的后台处理循环。 | 面向单次调用的同步服务。 |
| `Supervisor` | 管理 actor/worker 的启动、注册、恢复和退出边界。 | 单个会话的业务状态本身。 |
| `Owner` | 对一个可变生命周期/状态作出唯一串行决策的对象。 | 只负责广播或无状态转换的辅助器。 |
| `Handle` | 对单个资源的共享引用；名称应标出所引用资源，克隆会延长底层对象的存活时间。 | 资源集合、注册目录或生命周期管理器。 |
| `Executor` | 对已准入/已授权的目标执行一次操作，并拥有执行结果分类。 | 选择目标、授予权限或构造策略。 |
| `Handler` | 接收一个命令、事件或协议入口并转交给领域 owner。 | 跨多个页面/领域的大型编排器。 |
| `Controller` | UI 或应用入口的异步流程编排，依赖通过参数显式传入。 | 领域持久化 owner 或展示组件。 |
| `Reducer` | 由 action/event 和旧状态确定新状态的转换函数/对象。 | 网络、数据库或通知副作用的 owner。 |
| `Mapper` | 在明确边界转换字段/类型；每个 wire mapper 有唯一登记点。 | 负责业务决定或异步生命周期的 service。 |
| `Builder` | 用多个输入装配一个结果，且调用方能看出构造范围。 | 发布/应用该结果的 coordinator。 |
| `Factory` | 按显式参数/策略创建某类实例；若只封装单一 DTO 组装，优先叫 `Builder`。 | 运行期资源管理器。 |
| `Event` / `Command` | `Event` 表达已经发生的事实；`Command` 表达请求执行的意图。 | 彼此混用的通用 payload。 |
| `Bridge` | 仅为既有协议/生态术语保留；新跨边界类型优先使用更具体的 `Adapter`、`Mapper` 或 `Port`。 | 与这些角色并列但无独立语义的通用类型后缀。 |

### 函数动词

同一调用链中按**可观察语义**选动词；不能只为避免重名而替换同义词。下列规则适用于 Rust 方法与 TypeScript 导出函数，测试名按对应测试约定表达行为。

| 动词 | 含义 |
|---|---|
| `get` / `find` / `list` | 按稳定 key 取单项 / 按条件搜索 / 读取集合；缺失语义由返回类型体现，`get` 不暗示一定存在。 |
| `fetch` / `query` | 外部 I/O 读取 / 对本地或持久数据执行有条件查询。 |
| `load` / `restore` / `resume` | 读入运行态 / 从持久来源重建运行态 / 从保存的会话位置继续执行。 |
| `build` / `create` / `new` | 纯装配派生值 / 建立具有业务身份的实体或资源 / 普通构造函数。 |
| `prepare` / `apply` / `publish` | 生成未生效候选 / 对 owner 应用变更 / 将已提交版本暴露给消费者。 |
| `append` / `commit` / `persist` | 向追加式日志写入 / 原子提交一组变更 / 将状态写入持久存储。 |
| `register` / `activate` / `enable` | 加入注册表 / 纳入当前执行作用域 / 允许既有能力使用；三者不能互代。 |
| `update` / `replace` / `clear` / `delete` / `remove` | 局部修改 / 整体替换 / 清空集合 / 删除持久实体 / 从某个运行集合移除。 |
| `drain` / `pop` / `take` | 取出并清空队列或集合 / 移除并返回一个队头元素 / 转移或清空某个可选 owner 的值。 |
| `execute` / `run` / `handle` / `process` | 执行一次具名操作 / 推进一次流程或后台任务 / 接收并路由入口 / 消费或转换输入。 |
| `resolve` / `authorize` | 按 owner 与输入解析目标/契约 / 由安全 owner 作出允许、拒绝或确认决策。 |
| `emit` / `publish` / `send` | 发出事件 / 发布已提交状态 / 向外部端点或收件人发送消息。 |
| `map` / `project` / `normalize` / `parse` | 结构转换 / 从权威源派生视图 / 将宽松输入规整为契约 / 解析文本或 wire 格式。 |

数据库或领域查询即使按 session、subject、tag 等条件筛选，只要结果是零到多条实体，也使用 `list_*`（条件检索可使用 `find_*` / `search_*`）；`get_*` 留给单实体读取。缓存接口按稳定 cache key 读写一个缓存槽时仍可使用 `get_*`，即使槽内缓存的是集合。

这些词汇用于审计和迁移，不授权把不同的状态 owner、错误语义、事务边界或安全策略合并。发现名称相似时，先比较不变量、生命周期、失败行为和真实消费者；只有职责与权威来源相同才合并，否则保留边界并改成能表达作用域/角色的名称。

---

## 1. 后端 Rust

### 文件名 / 模块名
- **snake_case**，如 `stt.rs`、`openai_responses.rs`、`scheduled_tool_run.rs`。
- 目录即模块：`crates/tools/src/builtin/`、`crates/memory/src/repositories/`。
- crate 统一 `haven-{name}`：`haven-agent`、`haven-common`、`haven-memory`、`haven-tools`、`haven-llm`、`haven-input`、`haven-mcp`、`haven-skills`。

### 标识符
- 类型 / 枚举 / trait / 结构体 → **PascalCase**：`Modality`、`Intent`、`LlmConfig`。
- 函数 / 方法 / 变量 / 字段 / 模块 → **snake_case**：`fn detect_intent`、`stt_default_base_url`。
- 常量 / 静态 → **UPPER_SNAKE_CASE**：`MAX_SPEAK_CHARS`、`IMAGE_GEN_KEYWORDS`。
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

### 模块（`.ts`）
- 工具 / 状态模块 → **camelCase**：`streaming.ts`、`voiceSubmit.ts`、`markdownRenderer.ts`、`sessionStatus.ts`、`modelRoles.ts`。
- 前端逻辑模块、测试、Vite/Svelte 配置和 Node 工具脚本统一使用 TypeScript；Node 工具脚本使用 `.ts` 并由固定 Node 工具链直接运行。
- Svelte 组件脚本的目标形式为 `<script lang="ts">`；存量组件按域分批迁移，迁移时补齐参数、状态和 DOM 引用类型。
- UI 源码不新增 `.js` / `.mjs` 独立实现模块；迁移完成后，Svelte 组件也不再保留普通 `<script>`。
- 主要导出 Svelte store 的模块 → `xxxStore.ts`：`themeStore.ts`、`syncStore.ts`（`syncStore.ts` 导出同名的 `syncStore` 辅助函数，名随主导出）。
- 聚合 store 桶文件保留 `stores.ts` 命名（导出 `sessionStore`/`toolRunStore` 等命名导出）。
- IPC DTO 的前端 alias 放在对应领域的 `contracts/` 模块，已知字段从 generated command type 派生；确需开放扩展时显式叠加索引签名，不把稳定响应整体退化为 `Record<string, unknown>`。
- 常量 → **UPPER_SNAKE_CASE**：`SESSION_STATUSES`、`COLOR_MAP`、`ROLE_KEYS`。
- 局部变量 / 函数参数 → **camelCase**：`newKeyValue`、`reasoningOpen`、`ctxMenuItems`。

### 路由
遵循 SvelteKit 约定：`+page.svelte`、`+layout.svelte`。当前工作区只保留根路由，设置、工具和记忆通过根路由的 `?tab=` 查询参数切换，不保留旧的目录路由。

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
- **会话恢复统一叫 `resume`**：从历史打开会话、崩溃恢复、DB→气泡重建均用 `resume`（如 `get_session_for_resume`、`get_latest_session_for_resume`、`sessionResumeTargetStore`、`buildResumeMessages`、`SessionResumeResponse`）。会话恢复目标的类型带 `Session` 作用域前缀；禁止再用 `review` 指代该流程（`preview` 预览、code review 注释、工具 capability `"review"` 除外）。

---

## 4. 名词单复数

- **容器 / 集合 / 表 / 目录 / 仓库** → **复数名词**：`sessions`、`messages`、`tool_runs`、`facts`、`session_steps`、`memory_embeddings`、`modelCards`、`messages`。
- **单一实体 / 单行元素** → **单数**：`session`、`message`、`tool_run`、`row`、`card`、`msg`。
- **不可数 / 质量名词** 保持单数：`usage`、`audio`、`video`、`text`、`schema`、`kv_store`（复合词不数）。
- **前端 store 变量** 按承载实体命名（`sessionStore`/`toolRunStore` 可承载数组/对象，名字取实体单数，属约定）。
- **派生集合结果** 用「实体＋复数」或复数词，避免用裸形容词承载集合：写 `selectedSessions`、`filteredMessages`、`remainingMessages`、`keptExistingMessages`，不写 `selected`/`filtered`/`remaining`/`keptExisting` 指代数组。
- store `update` / `filter` / `map` 的回调单元素参数用单数短名（`m`/`x`/`row`/`card`/`t`），保持单数语义。
- **文件名 / 结构体名保持一致**：一个文件一个实体时文件名单数；实体本身为集合资源时文件名随结构体用复数：`files.rs` ↔ `FilesTool`、`tool_runs.rs` ↔ `ToolRunsTool`（启动名 `"files"`/`"tool_runs"`）。不可数域用单数：`memory.rs` ↔ `MemoryTool`（启动名 `"memory"`，覆盖 facts + items）。事实实体统一使用 `Fact` / `facts`，后台编排统一使用 `MemoryWorker`。
- 仓库 / 表名按所管理实体的复数命名，与其承载集合一致：`sessions.rs`、`messages.rs`、`facts.rs`、`session_steps`。
- 不可数名词文件（`usage.rs`、`media_audio.rs`、`text.rs`、`schema.rs`、`memory.rs`）保持单数。

> 该漂移已对齐：`scheduled_tool_run.rs`↔`ScheduleTool`、`env.rs`↔`EnvTool`（原 `env_var.rs`）、`system.rs`↔`SystemTool`（原 `SystemInfoTool`）。新代码避免再制造 `Xxx` 与文件名不同词的情况。

---

## 5. 命名自查清单（提交前）

- [ ] Rust 文件 / 模块 snake_case，类型 PascalCase，常量 UPPER_SNAKE
- [ ] 缩写整词统一（`stt`/`ocr`/`tts`），不混用别名
- [ ] 实体 ID 用 `{prefix}-{uuid32}`，经 `new_id` 生成
- [ ] Svelte 组件文件名 = 组件名（PascalCase）；已迁移组件脚本使用 TypeScript，模块 camelCase，store 尾缀 `Store`
- [ ] TypeScript 局部变量 camelCase，常量 UPPER_SNAKE
- [ ] 跨层只在边界转换 snake↔camel
- [ ] 会话恢复用语统一 `resume`，不用 `review`
- [ ] 工具调用、会话、ToolRun、后台工具运行、定时工具运行按本节口径使用
- [ ] 集合用复数、单元素用单数、派生集合不用裸形容词（`selected`→`selectedSessions`）
- [ ] 文件名与结构体/实体单复数一致（`file.rs`→`files.rs` 对应 `FilesTool`；仓库随表复数）
