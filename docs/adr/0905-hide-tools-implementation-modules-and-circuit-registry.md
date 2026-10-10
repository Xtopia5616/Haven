# 0905：收回 Tools 实现模块并通过 facade 重置工具熔断器

## 状态

已接受并实现（2026-10-11）。

## 背景

`haven-tools` 根模块把多项内部实现声明为 `pub mod`，但工作区生产消费者没有直接使用它们。公开 surface 包括 builtin 具体工具、工具 circuit registry、live-output hub、SkillRunner、桌面输入模拟、ToolRun 类型实现模块和 JSON 列表预算 helper。桌面输入模拟可直接调用 Windows `SendInput`，绕过 Tools 已有的授权执行入口。

App 的 `reset_tool_circuits` Tauri 命令还通过 `ToolsFacade::tool_circuits()` 取得具体 registry 并调用 `reset_all()`，跨过 facade 直接依赖 Tools 熔断器实现。`builtin` 具体类型与 SkillRunner 另有跨 crate 集成测试消费者，但不需要进入默认生产 API。`Tool::operation_spec` 的公开 trait 签名需要 `OperationSpec` 类型名；其字段和构造入口本来就是 crate-private，适合作为不透明契约类型保留。

## 决定

- 默认构建将 builtin 模块保持为私有；只有非默认 `test-support` feature 才将其开放给跨 crate 测试。`SkillRunner` 根导出同样只在 `test-support` 下可见。
- circuit、live-output、simulate、skill-runner、tool-run-types 和 util 模块均保持 crate-private。移除未使用的 `ToolCircuitRegistry` 与 `LiveOutputHub` 根导出；ToolRun 对外数据类型继续通过 crate 根导出，不要求消费者依赖实现模块路径。
- App 重置工具熔断器调用 `ToolsFacade::reset_tool_circuits()`。registry getter 只供 Tools 自身测试使用；其 map、锁和 `reset_all` 调用留在 Tools owner。
- 将 `operation_view` 实现模块设为私有，并通过 crate 根隐藏导出不透明 `OperationSpec`，满足公开 `Tool` trait 的方法签名。operation spec 的字段、创建和执行仍只由 Tools 使用。
- 删除没有消费者的旧快捷访问器；只用于单元测试的 circuit 检查和输入标签枚举标注为 test-only。

## 影响

- 默认生产依赖图不能再导入 builtin 实现、SkillRunner、熔断器 registry、live-output hub 或直接桌面输入模拟。App 只能通过 Tools facade 请求熔断器重置。
- Agent/App 的既有测试通过 dev-dependency 启用的 `test-support` 继续构造真实 builtin 工具；Tools 自身单元测试仍访问 crate-private 实现。
- 工具 catalog、授权、熔断行为、桌面输入行为、ToolRun wire DTO 和运行时结果保持不变。没有数据库、配置或 IPC shape 变化。
- `OperationSpec` 仍是公开 trait 签名类型，但只有根路径且 doc-hidden；不会重新开放实现 module。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 替代方案

- 保留 registry getter，只把 App 代码移进 adapter：拒绝，因为调用方仍可依赖具体 registry 方法与状态锁。
- 在 `ToolServices` 增加熔断器 port：拒绝，因为它会把属于工具执行 facade 的单一 reset 操作扩展为另一个共享 service，且当前只有一个跨 crate 调用点。
- 用 `cfg(test)` 隐藏 builtin 实现：拒绝，因为 Agent 的集成测试以普通 dev-dependency 编译，需要显式的非默认 `test-support` feature。
- 删除 `OperationSpec` 对外类型：拒绝，因为公开 `Tool` trait 的返回签名仍需要这个类型；本次只隐藏其实现模块。
