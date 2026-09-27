# ADR 0372：Settings 配置应用边界审计

- 状态：已采纳（2026-09-26）
- 基线：HEAD `cd5f69f`；开始时工作区干净
- 范围：Settings/model/provider 配置写入、`AppState` 配置所有权、运行时 apply plan、Settings IPC 写请求及失败/restart 语义；实现跟进覆盖同配置域 Tools admin writers
- 关联：ADR 0068、0235、0253、0323、0324、0337、0341、0351、0361、0368、0371

## 背景

路线图仍将 Settings apply 的补偿、失败重试、restart recovery 与跨 writer 并发标为未决。本审计继续沿现有实现检查是否存在第二个配置 owner、重复 target mapper、重复 Settings serializer，以及失败和 `restart_required` 信息是否由多个地方独立推断。审计只覆盖 Settings 与模型/provider 设置调用链；Tools admin writers 不纳入本切片。

## 证据与结论

### 配置存储与 runtime ownership

- `AppState` 只保留 Tauri 瞬时状态，并通过 `runtime: Arc<ApplicationRuntime>` 访问应用服务。`ApplicationRuntime` 持有一份 `Arc<ConfigService>` 和一个 `RuntimeConfigCoordinator`；没有第二个 `ModelManager` 配置容器。
- `ConfigService` 的 `Mutex<ConfigState>` 是进程内 live 配置 owner。`edit` 在锁内保存变更前快照、执行 mutation、计算 domain；变化时先调用 `ConfigLoader::save`，成功后才增加 version 并构造 `ConfigChanged`，释放配置锁后再发布通知。mutation 或 save 失败会恢复内存配置，不增加 version，也不发布通知。
- `ConfigLoader::save` 写入同目录、带进程号和序号的临时文件，再 rename 到配置路径；这避免直接覆盖时暴露部分 TOML，并为 path replacement 提供原子替换边界。实现未对临时文件和父目录调用 `sync_all`，因此不承诺断电场景下的稳定介质持久性。
- Settings 和 model commit-plus-apply 共用 `ApplicationRuntime::config_apply_gate`。`RuntimeConfigCoordinator` 拥有这个 gate、model edit 和 Router/media prepare/publish；`SettingsRuntimeApplyCoordinator` 仅拥有 Settings 有序 phase、phase 状态及失败/警告观测。Settings 的实际副作用仍交给各自 runtime owner。两个 coordinator 不持有重复的配置 snapshot 或 Router runtime。实现跟进由组合根创建唯一 `Arc<tokio::sync::Mutex<()>>`，同时传给 ApplicationRuntime 与 `AdminContext`；Tools admin 在 mutation service 内取得 gate，覆盖 durable edit 与 runtime side effect/catalog rebuild。授权与确认继续发生在 service 入口之前。Security 永久权限的 Tauri writers 也在保存和更新 live AuthorizationEngine 的临界区持有同一 gate。
- `ConfigService::changed_domains` 是字段到 config domain 的映射；`RuntimeConfigApplyPlan::from_change` 是唯一 config domain 到 runtime target 的映射；`SettingsApplyPlan` 从该 target plan 派生 phase 和诊断目标。Settings 复制的 `restart_required_targets` 只供同一事务的日志/观测，不是独立推断出的第二份策略。

### Model/provider 与前端请求

- `ModelSettings.svelte` 在 `SettingsView` 持有的表单草稿上编辑 provider、model catalog 和 request policy；一次 `update_settings` 保存该表单。保存 payload 只有 `SettingsView.saveSettings` 一个 builder。
- 聊天中的 `switch_model`、`set_reasoning_effort`、`set_web_search` 由 `ChatModelOperations` 调用，并通过同一 `RuntimeConfigCoordinator` 修改对应模型 slot、持久化后重建 Router。它们与 provider CRUD 使用不同编辑动作，但落入同一个 `ConfigService` 和共享 apply gate。`llm:config_changed` 供 UI 同步；它不重试已失败的 Router apply。
- `discover_models` 只查询模型目录，不持久化配置。`check_llm_connection` 读取 Agent 当前 Router 的连接状态，由 shell 的 `normalizeLlmConnectionReport` 单点归一化；这些路径不构成第二个配置写入或 apply mapper。
- Rust `haven_common::config::Settings` 继续拥有完整嵌套 schema。UI 的 `SettingsPayload` 保持开放形状以接受未来字段和枚举；本切片为唯一 builder 标注这一类型，并让 IPC script 核对 handler 的单一 `settings: Settings` 参数、Rust/TS registry 的 `Settings → unit/void` 契约及唯一 UI caller。脚本不复制或逐字段重建 Rust Settings schema。

### 保存顺序、失败与 restart

- Settings 在共享 apply gate 内调用一次 `ConfigService::edit`。durable edit 与 `ConfigChanged` 先于 runtime apply；Router/media prepare 使用该次 immutable snapshot。prepare 失败时持久化配置保留、live Router/media 保持旧 generation，后续 phase 不运行。Skills、logging 或 hotkey 等后续 fatal phase 失败时，已保存配置和此前成功的 live side effects 保留，协调器停止后续 phase；没有补偿或 rollback。
- Settings 对相同 payload 再提交是 no-op，不重跑 apply。model 对相同 mutation 再提交也不重建 Router；现有成功后的 UI event 不能作为 apply retry。当前没有针对已保存 snapshot 的显式 retry command，也没有 Settings 自动重启或启动恢复入口。
- `restart_required_targets` 来自共享 target plan，在提交日志和 phase failure observation 中重用；它只记录需重启 consumer，不会主动重启应用。仅变更 `SkillsExec` 时 Skills 只进入 restart-required，不执行 live Skills phase；若同时变更 `Skills` 与 `SkillsExec`，Skills 同时进入 live 与 restart-required，正常执行既有 Skills phase 并标记执行配置需重启生效。
- Settings durable edit 后的 runtime phase 失败保留磁盘配置，停止后续 phase，不增加 compensation/rollback 或自动 retry。命令错误以“部分 apply 失败”说明配置已保存和重启后从磁盘重新初始化；`SettingsView` 显示该状态并将表单基线更新为已保存值，避免把已持久化配置当作待重试写入。启动时仍由磁盘 `ConfigLoader` 初始化 runtime。model Router apply 失败同样保留 durable edit 并报告部分 apply 失败。
- Tools admin 的 MCP/Skills/Logging/ToolSettings 变更在实际 `AdminServices` mutation service 内共用该 gate。AdminServices 已完成 catalog rebuild，Tauri wrappers 删除重复 rebuild；`McpManager::connect_server` 已启动 monitor，因此删除重复 monitor start；Tauri 的 refresh/reconnect 保留其不同的手工 monitor 路径并在 gate 内完成。MCP update 仍维持 connect-before-persist 与既有失败恢复顺序。

## 实现跟进（2026-09-27）

产品决策已落地于 app composition root、Tools AdminServices 和 Settings UI。共享 gate 仍由 app-binary 所有，通过窄 `AdminContext` 注入 Tools，不下沉到 common，也没有反向依赖或全局 service locator。回归覆盖 Tools Admin durable write 等待共享 gate、Settings/model apply 互斥，以及纯 SkillsExec 与 Skills+SkillsExec 的 phase/restart 组合。配置格式、持久化顺序和 IPC 字段未变，无需配置或数据库重置。

## 决定

1. 保持 `ConfigService`、共享 apply gate、target mapper、Settings phase coordinator 和具体 runtime owner 的现有边界；没有发现需迁移的第二份配置真源或重复 target mapping。
2. 为 SettingsView 的唯一 payload builder 复用既有开放式 `SettingsPayload` 类型；扩展 IPC contract script 验证 `update_settings` 的 Rust 参数、registry 和直接 caller owner。command name、wire fields、错误文本及 UI 行为不变，也不增加 serializer 或 nested Settings 镜像。
3. 保持 durable-first、失败不补偿、相同输入不隐式重试；runtime apply 失败时保留磁盘配置、停止后续 phase，并在应用重启时从磁盘配置初始化。无自动 restart 或显式 retry。
4. 仅 `SkillsExec` 变更产生 Skills restart-required 并跳过 live Skills phase；Skills 与 SkillsExec 混合变更同时保留 live Skills phase 和 restart-required。UI 报告配置已保存但部分 runtime 未应用；不把 `restart_required` 推断为自动 restart，也不按相邻状态推断 apply 成功。

## 后续产品决策（2026-09-26）

产品已明确 Settings apply 失败语义：配置写盘成功后，运行时后续阶段失败时保留 durable
配置并报告“部分 apply 失败”；不自动重试，应用重启时从磁盘配置重新初始化。该决定不改变
本审计记录的 owner、gate、phase 顺序。具体文案与实现已在 2026-09-27 实现跟进中落地。

另外确认两项运行时配置方向：仅修改 `SkillsExec` 时只保存并标记重启后生效，不执行 live
Skills apply；Settings/model apply 与 Tools 管理操作若写入同一运行时配置域，必须统一串行。两项
决定当时不扩展 raw Database/ToolsManager 收口范围，后续 runtime 实现见上方实现跟进。

## 验证

- 本实现跟进的完整 Rust/UI/IPC 门禁结果记录在提交摘要；本次变更涉及 Rust runtime 与 UI，须执行两个 workspace 的适用门禁。

## 回滚

回退实现跟进时恢复原 writer gate、SkillsExec phase 选择与 UI 错误呈现，并同步回退本 ADR/路线图/架构记录。配置格式、数据库和用户数据均无需重置。
