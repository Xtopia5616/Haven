# ADR 0162：ToolsManager 的 core/runtime/builtins 边界

- 状态：accepted
- 日期：2026-09-15
- 范围：`haven-tools`、`haven-agent`、`haven-app-binary`

## 背景

`ToolsManager` 同时持有工具契约、目录、授权、MCP/Skills、媒体 provider、
ActionService、admin surface 和应用服务注入点。它还曾以 closure、可变
`Option` 和 `Weak<ToolsManager>` 作为隐式 service locator，使 catalog rebuild
期间的依赖来源不清晰，并允许不同 catalog generation 看到不同的反向服务。

## 决定

1. `haven-tools/src/tool_core.rs` 的 `ToolCore` 只拥有 contract/catalog/
   authorization 相关状态：registry、deferred/session catalog、tool settings、
   context limits 和 circuit registry。
2. `haven-tools/src/tool_runtime.rs` 的 `ToolRuntime` 只拥有执行所需的运行时
   capability：取消边界下的 action/live output、媒体和 asset 依赖、router、
   admin surface 以及 typed capability ports。
3. `haven-tools/src/tool_builtins.rs` 的 `ToolBuiltins` 只拥有 MCP/Skills/
   shell 等具体 builtin provider，并负责把三层依赖组装成不可变的 `BuiltinContext`。
   `ToolsManager` 保留为 catalog/composition facade，不再公开可替换字段。
4. Agent spawn/lifecycle 使用 `MessagingRuntime`，memory recall 使用
   `MemoryRecallPort`，tool toggle 使用 `ToolControlPort`，日志热更新使用
   `LogLevelPort`。这些 port 在组合根只绑定一次；服务不再接受 `Arc<dyn Fn>`
   或可变 callback slot。生命周期需要弱引用时，弱引用只存在于具体的 typed
   handle 内，不出现在 builtin context 的通用字段中。
5. `MessagingService` 和 memory recall slot 使用 `OnceLock`。运行时绑定失败
   显式返回错误，不能静默替换已经工作的 capability。

## 替代方案

- 立即拆成三个 workspace crate：会把所有 builtin 对 memory/MCP/LLM/input 的
  依赖同时迁移，扩大循环依赖和回滚面；先在 `haven-tools` 内建立模块边界，
  待接口稳定后再提升为 crate。
- 继续在 `ToolsManager` 上公开服务字段：调用点短，但任何调用方都能绕过
  catalog facade 直接替换或交叉使用服务。
- 用新的 closure 包装旧 callback：名称改变但 service locator 语义不变，故不采用。

## 影响

- 外部调用通过 `registry()`、`authorization()`、`mcp_manager()`、
  `skills_engine()`、`action_service()` 等明确 domain accessor；catalog rebuild
  的依赖组装集中在 `ToolBuiltins::build_context`。
- 不改变工具名称、provider schema、数据库、配置和 IPC 契约；memory recall、
  peer messaging、tool toggle 和动态日志等级保留原有行为。
- 这是进程内架构重构。未来拆 crate 时应保持三层依赖方向：core 不依赖 runtime
  或 builtins，runtime 不依赖 agent，builtins 只通过 typed ports 使用上层能力。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --locked -p haven-tools --lib`
- `cargo test --locked -p haven-agent --lib`

## 回滚与重置

不改变持久化 schema、配置 schema 或 IPC wire payload；回滚代码即可，无需用户
数据重置。回滚时必须同时恢复三层字段访问、typed port 绑定和所有调用方 accessor，
不能只移除新模块。
