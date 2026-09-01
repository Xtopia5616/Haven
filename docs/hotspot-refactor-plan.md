# Haven 大文件与热点拆分执行计划

> 用途：把当前代码审计结果交给后续 agent，按稳定职责拆分大文件；允许破坏性结构重构，但保持行为、IPC、持久化和安全契约不变。
>
> 本文是执行计划，不授权新增功能或顺手清理无关代码。每个目标应独立完成、独立验证、独立提交。

## 1. 执行前必须阅读

执行 agent 开始前必须阅读：

- `AGENTS.md`
- `docs/development-standards.md`
- `docs/architecture.md`
- `docs/refactor-execution-guide.md`
- `docs/stability-refactor-plan.md`
- 与当前目标相关的 `docs/adr/` 文件

开始时运行 `git status --short`、`git diff --stat` 和 `git diff`，保留用户已有修改，不覆盖或代提交无关变更。

## 2. 审计结论

统计排除了 `target/`、`node_modules/`、`ui/build/` 和 `ui/.svelte-kit/` 等生成目录；行数包含注释和空行，仅用于定位热点。

| crate | Rust 总行数 | 生产代码 | 测试代码 | 源文件数 |
|---|---:|---:|---:|---:|
| `haven-agent` | 31.4k | 18.3k | 13.1k | 46 |
| `haven-tools` | 29.0k | 17.0k | 12.0k | 34 |
| `haven-llm` | 21.0k | 12.3k | 8.7k | 32 |
| `haven-memory` | 14.7k | 8.7k | 5.9k | 21 |
| `haven-app-binary` | 8.9k | 7.2k | 1.7k | 22 |
| `haven-mcp` | 2.4k | 2.0k | 0.4k | 2 |

最大的非代码文件是 `assets/models/silero_vad.onnx`（约 2.7 MB），它是模型文件，不进行代码拆分。

结论：当前优先做文件级拆分，不把 `agent`、`tools` 或 `llm` 直接拆成新 crate。它们已经按领域拥有较多子模块；贸然拆 crate 会扩大依赖、公共 API 和测试迁移范围。

## 2.1 兼容层原则

- 这是测试版项目，允许为了清晰的最终边界进行破坏性重构；不要为了保留旧的内部导入路径而长期维护 re-export、代理函数或双入口。
- workspace 内部调用点应在同一轮重构中迁移到新模块。不能新增依赖旧 facade 的代码，也不能把 facade 当作永久 API 设计。
- 如果拆分过程中确实需要 facade，它必须在文档或 ADR 中写明：用途、受影响调用点、删除条件和预计删除轮次；没有删除条件的 facade 不得保留。
- “完成”不等于“旧入口还能工作”。完成标准是调用方已迁移、旧入口已删除，或有明确且必要的外部稳定 API 理由。

## 3. 执行顺序

### 阶段 A：先拆测试集中文件，低风险

目标：[crates/agent/src/integration_tests.rs](../crates/agent/src/integration_tests.rs)

- 规模：约 5,185 行，全部是测试。
- 按职责拆成多个测试模块，建议至少分为：
  - 生命周期、消息持久化与 resume/rollback
  - 工具参数验证与 confirmation 恢复
  - ReAct 核心循环、steering、ask、暂停/恢复
  - 工具批次、并发、取消和终态投影
- 把共享 mock、数据库夹具、emitter 和测试工具集中到一个 `support` 模块，避免复制。
- 保留当前测试的可见性和测试名称；不要因为移动文件而删除覆盖场景。
- 这是优先级最高、行为风险最低的一步。

验收：

```powershell
cargo test --locked -p haven-agent
```

### 阶段 B：拆 MCP 单文件实现

目标：[crates/mcp/src/lib.rs](../crates/mcp/src/lib.rs)

- 规模：约 2,241 行，其中约 1,891 行是生产代码。
- 当前混合了四类职责：
  - MCP/JSON-RPC DTO、请求构造和 content block 提取
  - stdio 与 Streamable HTTP transport、SSE 读取和进程启动
  - 单服务器 `McpClient`、限流、重连和健康监控
  - 多服务器 `McpManager`、配置 reconcile 和 `McpToolCaller` 适配
- 建议拆为 `protocol.rs`、`transport.rs`、`client.rs`、`manager.rs`；已有的 `sse.rs` 继续保留。
- `lib.rs` 最终只保留模块声明和真正需要的公共导出。workspace 内部调用方应迁移到新模块；如果阶段性保留旧导出，必须遵守 §2.1 并在本阶段末删除，除非它确实是外部稳定 API。
- 不改变 MCP wire shape、`Mcp-Session-Id`、stdio 进程回收、健康监控、限流或二进制 payload 上限。

验收：

```powershell
cargo test --locked -p haven-mcp
cargo test --locked -p haven-tools --test mcp_integration
```

### 阶段 C：拆后台任务与 shell 辅助模块

目标：[crates/tools/src/bg.rs](../crates/tools/src/bg.rs)

- 规模：约 2,658 行，其中约 1,682 行是生产代码。
- 建议按以下边界拆分：
  - `shell_runtime.rs`：shell 命令构造、PowerShell 编码、代理探测、输出日志路径
  - `background_actions.rs`：`BackgroundActions`、状态机、action registry、事件 sink
  - `output.rs`：输出收集、UTF-8/GBK 处理、CLIXML/ANSI 清洗、错误摘要和 Windows 诊断
  - 必要时再把进程树终止和 live tail 读取放到 `process.rs`
- `bg.rs` facade 只允许作为临时迁移措施，不是目标架构。优先在同一轮中直接迁移所有 `crate::bg::*` 调用点并删除它；只有在拆分过程中确实需要分步编译时，才短暂保留 `bg.rs` 的 `pub use`。
- 如果暂时保留 `bg.rs`，必须在该提交/ADR 中写明删除条件；不得新增对 facade 的调用，阶段完成前应再次搜索调用点并删除 facade。不能以“兼容性”作为长期保留理由。
- 不改变 `CREATE_NO_WINDOW`、PowerShell `-EncodedCommand`、输出容量上限、日志落盘、取消和进程树终止语义。
- Windows 专属路径必须继续保留对应的条件编译和负向测试。

验收：

```powershell
cargo test --locked -p haven-tools
cargo clippy --workspace --locked -- -D warnings
```

### 阶段 D：拆 Tool contract、registry 和安全网关

目标：[crates/tools/src/tool.rs](../crates/tools/src/tool.rs)

- 规模：约 2,237 行，其中约 1,287 行是生产代码。
- 当前混合了：
  - `Tool`、`ToolResult`、`ToolSignals`、`ToolExecutionOutcome`、重试/并发契约
  - `ToolRegistry` 和 session catalog
  - `SafetyGateway`、权限继承、disabled operation、路径沙箱和 reparse point 检查
- 建议拆为 `tool_contract.rs`、`registry.rs`、`security.rs`；workspace 内部调用方直接迁移到新模块。只有确实属于外部稳定 API 的导出才保留，不能为旧内部路径长期维护薄 facade。
- 安全模块拆分时必须先建立目标接口，再迁移完整调用链；不能把安全检查复制到各 builtin。
- 不改变 deny 优先级、权限继承、路径规范化、UNC/device path 拒绝、超时未知终态和操作幂等性语义。
- `LOCAL_TOOL_SECURITY_MATRIX` 应继续只有一个权威来源，并保留安全回归测试。

验收：

```powershell
cargo test --locked -p haven-tools
cargo test --locked -p haven-agent
cargo clippy --workspace --locked -- -D warnings
```

### 阶段 E：收窄 app-binary 组合根

目标：[crates/app-binary/src/lib.rs](../crates/app-binary/src/lib.rs)

- 规模：约 1,925 行，其中约 1,368 行是生产代码。
- 当前混合了：
  - `TauriEmitter` 与 AgentEvent → IPC payload/channel 映射
  - `HavenShellHandler`、`HavenInputHandler` 宿主适配
  - Tauri 启动、托盘、全局快捷键、单实例和后台初始化
  - shortcut/tray 等辅助函数
- 建议抽出 `event_bridge.rs`、`handlers.rs`、`bootstrap.rs`；`lib.rs` 只作为组合根和 `run()` 入口。
- 事件映射必须保持单一登记点；每个 wire DTO 的 snake_case/camelCase 边界不能被拆散。
- 不改变启动顺序、后台初始化、托盘唤醒、快捷键录音生命周期、通知双通道和 session/action 事件形状。

验收：

```powershell
cargo test --locked -p haven-app-binary
cargo check --workspace --locked
```

### 阶段 F：UI 视图拆分

这些目标应在 Rust 热点完成并稳定后处理。

#### Settings

目标：[ui/src/lib/views/SettingsView.svelte](../ui/src/lib/views/SettingsView.svelte)

- 规模：约 1,705 行，script 部分约 1,041 行。
- 建议拆为设置页外壳/离开保存流程、General 设置、Limits 设置；模型和媒体设置继续由已有 `ModelSettings.svelte` 承担。
- 配置 snapshot、dirty 检测、远端默认模型 reconcile 和保存流程应集中在一个明确的状态边界，不要在多个组件双写。

#### Model settings

目标：[ui/src/lib/views/ModelSettings.svelte](../ui/src/lib/views/ModelSettings.svelte)

- 规模：约 1,480 行。
- 将 provider/model role 配置与 STT/OCR/TTS/image generation 媒体配置拆成两个视图或子组件。
- 保持模型发现、api style、key 状态、默认模型同步和能力灰显行为不变。

#### Memory

目标：[ui/src/lib/views/MemoryView.svelte](../ui/src/lib/views/MemoryView.svelte)

- 规模：约 1,293 行。
- 按现有 tab 拆为 session history、long-term facts、memory recall 三个子视图。
- 保持分页/搜索/删除/导出、事实来源筛选、resume，以及 session message/usage store 的单一写入路径。

UI 验收：

```powershell
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

### 阶段 G：低优先级复杂操作文件

目标：[crates/tools/src/builtin/self_tool.rs](../crates/tools/src/builtin/self_tool.rs)

- 规模：约 2,787 行，其中约 1,420 行是生产代码。
- `SelfOperation` 同时覆盖 config、skills、tools、MCP、logs、sessions/errors。
- 后续可按 config/skills、MCP、diagnostics/history 拆 handler 模块；保留一个 dispatcher。
- 这是高风险目标，必须先补齐每个 operation 的正向、错误和持久化测试，不要作为第一轮拆分。

## 4. 暂时不要做的事情

- 不把 `haven-agent`、`haven-tools`、`haven-llm` 直接拆成多个 crate。
- 不因为 `openai.rs`、`openai_responses.rs`、`anthropic.rs` 各约 2.5k 行就立即拆 provider crate；每个文件约一半是协议测试，先考虑把测试按 provider 移到独立测试模块。
- 不拆 `memory/src/repositories/facts.rs` 的生产 facade；它总计约 2,080 行，但生产代码约 507 行，图谱写入、查询和维护已经分别位于其他模块。
- 不修改 ReAct X12 写路径、`ReActSnapshot.events` 恢复权威、消息/步骤投影、rollback 双时钟或任何数据库 schema。
- 不借拆分机会修改 provider wire payload、工具重试、安全确认、IPC event shape 或 UI 交互。

## 5. 可选的 crate 级后续方向

如果完成上述文件拆分后仍需要降低 `haven-tools` 的跨域耦合，可以另立任务评估 `haven-tool-core`：

- 放置稳定的 `Tool`、`ToolResult`、`ToolExecutionOutcome`、`ToolConcurrency`、`ToolRegistry`、`SafetyGateway` 契约。
- `haven-tools` 保留 builtin、background action、MCP/skill adapter 和具体执行逻辑。
- 这是独立的 crate/API 重构，必须单独写 ADR、迁移调用方并跑完整 workspace 门禁；不要和本计划的文件拆分混在一个提交中。

## 6. 每个拆分目标的完成标准

1. 生产行为和测试行为不变；移动测试不能减少覆盖场景。
2. 新模块职责单一，原入口文件只保留真正需要的公共导出或组合编排；临时兼容 facade 不算完成，除非已记录删除条件和必要性。
3. 没有新增反向依赖、循环依赖、重复实现或第二个契约真源。
4. 相关 ADR/架构文档在确实改变边界时同步更新。
5. 至少运行目标 crate 的测试、workspace 编译和严格 Clippy；跨端目标额外运行 UI check/test/build。
6. 提交前运行 `git diff --cached --check`，精确暂存路径，并使用符合规范的 `refactor(...)` 提交。

## 7. 全部阶段完成后的门禁

```powershell
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

按项目规范，仍保留在约 800 行以上的文件必须在对应 ADR 或变更说明中写明保留理由，不能仅以“历史文件”作为理由。
