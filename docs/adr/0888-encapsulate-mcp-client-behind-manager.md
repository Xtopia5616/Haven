# ADR 0888：将 MCP client 状态、生命周期与调用收口在 McpManager

## 状态

Accepted — 2026-10-10

## 背景

`McpClient` 实际位于私有模块，但 `haven-mcp` 将它从 crate 根公开，`haven-tools` 又继续 re-export；`McpManager::get_client` 因而把具体 client 交给 catalog、load_mcp、管理服务和 App command。调用方直接读取状态与工具缓存、调用工具、比较配置，并自行启动健康监视器。连接、状态事件、缓存和 monitor 生命周期由多个层次共同承担，manager facade 无法阻止新的绕行。

沿调用链还发现两个实现缺陷：手动 reconnect 会让 manager 与 AdminServices 都启动 monitor；配置变化重建 client 时曾直接从 map 删除旧 client，跳过 monitor 取消和 transport shutdown。管理器统一持有 monitor handle 并通过同一个 remove 路径关闭旧 client 后，这两类生命周期分叉一并消除。

## 决定

- `McpManager` 是 MCP client 的唯一跨 crate facade，拥有 live client 集合、连接与重连、健康 monitor、工具发现缓存、状态广播和工具调用。具体 `McpClient` 不从普通生产 API 导出，client map 与 getter 只在 manager 内部可见。
- 将生产消费者所需的 presence、status、snapshot、配置匹配、工具缓存/等待、连接协调和调用表达为 `McpManager` 的领域操作。Tools 的适配器模块与具体 adapter 收为内部实现；`McpToolAdapter` 持有 manager clone，通过 manager 调用工具，不再持有 client。
- monitor handle 由 manager 按 server name 保存；安装时替换旧 handle，重连前停止旧 monitor，移除或关闭时停止对应 monitor。配置 reconcile 统一经 `remove_client`，由同一 owner 取消任务并 shutdown transport。
- App 授权只需要规范的 skill tool name；该映射由 `skill_tool_name` 暴露为稳定领域函数，不再通过公开 `SkillToolAdapter` 类型取得。
- 低层 `McpClient`、adapter 类型和 panic 测试构造器仅通过非默认 test-support 或 crate 单元测试暴露；跨 crate 测试依赖显式启用 feature。生产调用方不能以测试 API 替代 facade。

## 替代方案

继续公开 client 并要求调用方自觉使用 manager。拒绝：文档约定不能阻止 status、cache、reconnect 和 monitor 生命周期被分散到新消费者；隐藏 getter 与实现类型才能让绕行在编译阶段失败。仅把 `McpClient` re-export 移除但保留 `get_client` 也不够，因为返回实现的 getter 会继续泄漏具体类型。

## 影响与兼容性

这是测试版本内的 Rust API 破坏性变化，不保留 client 或 adapter 的兼容 re-export。MCP 配置、SQLite、Tauri IPC payload 与 provider 协议不变，不需要重置数据。连接能力和工具发现结果仍由同一 client 实现提供；monitor 生命周期与重建路径改由 manager 独占，重复 monitor 不再并行运行。

## 验证

`cargo check --workspace --locked`、`cargo fmt --all -- --check`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings`。仓内生产调用点静态复核不再通过 `get_client`、`McpClient` 或 `status_tx` 操作具体 client；低层测试只通过显式 test-support feature 使用。

## 回滚

若新增正式消费者需要新的 client 行为，将该行为定义为 `McpManager` 领域操作；只有当出现真实独立业务协议时才增加领域 port。不得恢复 `get_client`、client re-export 或公开 monitor 启动入口。
