# ADR 0685：移除 Agent prompt 的 eager registry fallback

## 状态

已采纳并实施。

## 背景

Agent prompt catalog adapter 先从 `ToolsFacade::list_enabled_builtin_tool_definitions` 读取已发布的 builtin catalog；结果为空时又从 `ToolRegistry::list_tool_definitions` 读取另一份列表。后者不是同一 catalog 的空状态：它绕过 enabled 过滤、deferred operation definitions 与 catalog publish 边界。生产组合只在 `rebuild_catalog` 中同时建立 registry 与 builtin catalog；当前 workspace 没有生产调用方单独向共享 registry 注册 prompt 工具。旧 fallback 也不能填补真正的启动空窗，因为 catalog 尚未初始化时 registry 同样为空。

`PromptCatalogVersions.registry` / `SchemaCache.registry_version` 则把全局工具目录代次称作 registry 版本；MCP 与 Skills 代次又以另外两个裸字段并列保存，cache lookup 通过三个位置参数比较，目录 owner 不够明确且组合容易错位。

## 决定

- 删除 App adapter 与 Agent 测试 adapter 中从 ToolRegistry 回退读取 prompt definitions 的分支。
- prompt cache 版本统一表示为 `PromptCatalogVersions { global_catalog_version, mcp_catalog_version, skills_catalog_version }`；全局代次读取 `ToolsFacade::catalog_version()`。
- `SchemaCache` 以一个 `catalog_versions` 字段保存这组版本；cache lookup 接收整个版本结构并整体比较。
- 保留 ToolRegistry 作为执行注册表；已注册但未进入已发布 builtin catalog 的条目不属于 prompt index。
- prompt 测试使用明确返回 catalog content 的 port fixture；App adapter 的负向覆盖确认 registry-only 项不会被当成已发布 catalog 内容。

## 替代方案

- 保留“目录为空时试读 registry”：拒绝。它引入第二数据源，且输出不满足 prompt catalog 的 enabled/deferred 语义；当前生产启动顺序也没有独立 eager registry 状态可供恢复。
- 把三个版本继续作为独立标量传入并分别存储：拒绝。它们共同定义同一个 prompt cache key，使用命名版本结构可减少位置错配并明确每个 owner。

## 影响与验证

- 只影响 Agent prompt 内部适配和 cache key 结构，不改变 provider 工具声明、IPC、配置或持久数据，无需重置。
- 已执行 Rust workspace check、严格 Clippy、格式检查、ADR 索引与差异检查；按本轮工作约束未运行测试。

## 回滚

可恢复旧 fallback 和散列版本字段，不涉及数据恢复或配置迁移。
