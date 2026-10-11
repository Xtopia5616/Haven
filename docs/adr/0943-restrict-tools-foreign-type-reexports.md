# 0943：将 Skills 查找与执行收回 Tools owner

## 状态

已接受并实现（2026-10-11）。

## 背景

生产 `SkillCatalogPort` 曾向 App 返回完整 `haven_skills::Skill`，App 随后把同一对象交还 `SkillExecutionPort`。执行器和 registry 都由 Tools 持有，这让 Skills runtime 对象跨越 owner 边界并增加了一个无行为的 App adapter。与此同时，Tools root 无条件导出没有消费者的 Skills `Language` 类型；MCP `McpToolInfo` 只供 test-support adapter 构造和 Agent 集成测试使用，却在默认生产 API 中公开。

## 决定

- `SkillCatalogPort` 只提供配置、刷新与 `SkillInfo` catalog 查询，不再返回完整 `Skill`。
- `SkillExecutionPort` 接收 skill name、参数与取消令牌，由 Tools 在自身 registry 中解析并执行该 Skill；App 直接持有这个窄 capability，不再重复包一层透传 adapter。
- 仅测试使用的 fixture lookup 放在 `ToolCatalogTestSupportPort`，不进入默认生产能力。
- 删除 Tools root 对 Skills `Language`、`Skill` 的 re-export；将 MCP `McpToolInfo` root re-export 限定到 `test-support` 和 Tools 自身单元测试构建。
- 保留 `SkillInfo` 作为 Tools skill catalog port 的公开 DTO，并保留面向 App 的 MCP 状态 DTO。

## 影响

App 的 skill execution 内部调用改为传名称，Tool UI/IPC contract、目录数据、授权 key、执行参数、持久化和安全语义保持不变。仅测试构造入口通过 `test-support` 取得 fixture。无需数据重置。

## 验证

- 静态搜索确认 App 不再接收或传递 Skills runtime `Skill`，且生产 `SkillCatalogPort` 不暴露它；`Language` 无 Tools 路径消费者，`McpToolInfo` 仅出现在测试适配器边界。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引及 `git diff --check`。

## 替代方案

- 保留 App 的 `get_skill` 后再调用 execution port：拒绝。registry lookup 与进程执行属于 Tools/Skills owner，拆在 App 两次调用会跨边界传递实现对象。
- 为 Skills 和 MCP 所有值类型复制一套 Tools DTO：拒绝。`SkillInfo` 和 MCP 状态 DTO 已是窄查询 capability 的数据形状；复制会新增同步和转换责任。

## 回滚

若 App 未来确需新的 Skills runtime 能力，应由 Tools 暴露按领域命名的操作；不要重新返回底层 `Skill` 对象或把 Skills registry 暴露给调用方。
