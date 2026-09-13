# ADR 0138：工具链路统一与运行时生命周期边界

## 状态

已接受（2026-09-13）

## 背景

内置工具已经具备统一的 operation view、风险、并发、超时和结果契约，但定义、构造、注册、授权、执行、错误输出及长任务仍有多个局部入口。局部入口会导致同一失败被重复分类、输出截断后丢失恢复字段，以及配置变化时无关工具被反复销毁。工具后端的统一目标是统一链路语义，而不是让所有业务工具共享同一个实现。

## 决定

1. `builtin::BuiltinContext` 作为内置工具装配入口；媒体依赖和长任务依赖分别收敛到 `MediaDeps`、`ActionDeps`。新增依赖只进入对应分组，不再继续扩大位置参数列表。
2. `ToolResult` 携带 `error_class`、`outcome` 和 `retryability`。`ToolsManager` 不再扫描错误文本；实现可通过 `Tool::error_metadata` 或 typed operation 的 `error_metadata` 提供机器可读失败语义，跨 object-safe 边界时使用 `StructuredToolError` 携带 metadata。没有证据时按不可重试/未知结果处理。
3. `OutputBudget` 是工具观察值的统一入口，`ToolOutput` 负责生产端把截断标记和完整日志路径与结果绑定。具体工具仍可有更小的 provider/业务上限，但最终模型观察必须经过统一预算。
4. `ToolRegistry` 的名称索引和有序列表必须保持唯一；`register`、`rebuild` 遇到重复名称返回错误并保持原快照不变。需要替换时由一次原子 `rebuild` 完成，而不是隐式覆盖。
5. 目录重建按受影响的根 operation 进行。装配代码仍会生成当前能力声明，但未受影响且同名的运行时实例会复用；因此 provider、action adapter 和其他有状态依赖不会因无关配置变化而被拆掉。注册表替换仍是原子操作。
6. 进程型后台任务和定时任务保留各自的业务状态、持久化、取消和终态语义；只抽取共同的事件 sink 基础设施 `ActionLifecycle`。两者不是同一种 action，禁止为了代码复用合并成一个状态机。
7. 外层工具超时由 `Tool::execute_with_timeout`/manager 负责；HTTP、媒体、Skill、文件摘要等 provider 只负责自己的请求或子操作 deadline，并预留边界余量。超时结果必须显式区分已终止和结果未知，不能由错误文本推断。
8. 内部稳定接口使用 typed request。`FileSearchEngine` 现在直接接收 `SearchRequest`；JSON 只停留在 provider/tool 边界。

## 未选择的方案

- 不把 `TypedToolAdapter` 改成所有工具的万能 dispatcher；复杂的 oneOf、流式、会话副作用和 native facade 仍保留专用实现。
- 不把所有 `self` 管理操作或两种长任务强行合并；后续拆分以稳定领域边界为准，避免一次性移动大量相互依赖的测试夹具。
- 不用 `upsert` 掩盖注册冲突；重复名称是装配错误，必须在构造阶段暴露。

## 影响与回滚

这次变更只调整工具运行时内存结构和错误/输出 wire 语义，不修改数据库 schema。旧的 `ToolResult` JSON 缺少新增字段时按 serde 默认值读取。若发现运行时装配问题，可回退本 ADR 对应提交；不会触发数据迁移。生产环境异常时优先保留旧注册表快照，随后修复冲突来源。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked --workspace`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`
- `cd ui && corepack pnpm run check && corepack pnpm run test:run`
