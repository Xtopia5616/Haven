# 0900：将 ToolRegistry 与 OperationRegistry 隐藏在 Tools owner 内

## 状态

已接受并实现（2026-10-10）。

## 背景

`ToolsFacade` 曾公开 `registry()` 和 `operations()`，使 Agent、App 及其它 crate 能直接取得可变 installed registry 或完整 operation registry。生产代码没有跨 crate 使用这两个 getter；Agent/App 中的真实消费者全部是测试夹具。具体注册表类型还通过 crate root 重新导出，按需加载器的公开模块和字段也暴露了这些内部集合。这样调用方可以绕过 Tools 发布的 catalog、session overlay 和 owner 操作。

## 决定

- `ToolRegistry`、`OperationRegistry`、`DeferredToolCatalog`、`SessionToolOverlay` 与 `RegistryProbe` 仅作为 Tools 内部类型；不再从 crate root 导出。
- `ToolsFacade` 不再对跨 crate 调用方暴露 registry 或 operation registry getter。生产消费者使用已发布的 `ToolCatalogSnapshot`、`ToolCatalogVersion` 和具名 facade 操作。
- Agent/App 测试使用非默认 `ToolCatalogTestSupportPort` 准备 installed/session catalog fixture；该端口不进入普通生产构建。
- 普通生产构建不携带 installed registry 中无生产消费者的独立 version counter；catalog invalidation 只使用已发布的 `ToolCatalogVersion`，避免同一 global tool catalog 同时存在两个容易分歧的时钟。Tools crate 的 `cfg(test)` 构建保留仅供单测断言重建行为的计数器，不进入产品或跨 crate 测试 port。
- `load_skill`、`load_mcp` 与 `tool_catalog` 是 Tools 内部实现模块，不再公开其具体工具构造器及 registry 字段。
- `ToolsFacade::register_for_session` 只供 Tools crate 内部单测使用；跨 crate 测试可通过 test-support 建立 fixture，生产恢复与加载仍走具名 loader/use-case。
- `SessionToolOverlay` 不再返回内部 registration/version map 的共享锁。预算预览、预算内原子批量注册、版本递增都由 overlay 自己执行；MCP、Skill、builtin loader 与 resume 注册复用这一 owner 操作。删除了原来由 `load_mcp` 手动读锁、预算、插入、bump 的平行路径。
- 长期规则：消费方按所需能力调用 owner，不取得 owner 的可变实现对象；仅领域职责明确且不暴露内部实现的 owner handle 可跨 crate 保留。见 `docs/development-standards.md`。

## 影响

- provider 工具定义、执行与 session overlay 的可观察运行时行为不变；并行 loader 的预算检查仍在写锁内原子完成，超额时整批拒绝。只有测试夹具能直接向 installed registry 注入工具。
- IPC、数据库、配置、事件、安全和恢复契约不变，无需数据重置。
- 默认 Rust API 有意破坏性收口，仓库没有兼容要求。
- Tools crate 内部单元测试仍可通过 crate-private 测试入口检查实现；跨 crate 测试只能使用明确的测试能力，不能依赖具体 registry 类型或任意 session registration 方法。

## 验证

以下门禁于 2026-10-10 在固定 Windows 工具链通过：

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`（无编译警告）
- `cargo test --workspace --locked`（通过；Tools 805 passed、Agent 619 passed，另有项目中已有 ignored 测试）
- `cargo clippy --workspace --locked -- -D warnings`

## 替代方案

- 只把 getter 改名或限制为只读 registry：拒绝，因为它仍让调用方依赖实现数据结构，并绕过已发布目录语义。
- 让 Agent/App 共用一个通用 catalog mutation port：拒绝，因为生产路径没有注册需求，通用变更能力会扩大权限面。
- 保留 getter 仅供测试调用：拒绝，因为测试构造需求不应决定普通生产 API 的可达性，使用单独的 `test-support` port 即可表达该差异。
