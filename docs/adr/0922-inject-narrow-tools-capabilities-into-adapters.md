# 0922：在 Tools composition adapter 中固定窄 capability

## 状态

已接受并实现（2026-10-11）。

## 背景

`ToolsFacade::share_services()` 返回整个 `ToolServices` capability bundle，供组合根在构造时分配服务。`ApplicationRuntime::new` 已取得 bundle，但 App ToolRun、Skill execution 与 live-output adapter factory 又接收 `ToolsFacade` 并重新读取整包。Agent prompt adapter 也持有 ToolsFacade，并在每次读取 catalog versions/content 时重复读取整包只为访问 Skills catalog。

这使运行期 adapter 依赖宽 bundle 获取路径，而不是依赖构造时已选定的能力；也让以后修改 `ToolServices` 时波及与变更无关的 adapter。

## 决定

- AppRuntime 在 composition 阶段读取一次 `ToolServices`，并将 `SkillExecutionPort`、`ToolRunManagementCapability`、`LiveOutputSinkPort` 分别传给对应 App-owned adapter；adapter factory 不再接收 ToolsFacade。
- Agent adapter 构造时缓存其需要的 `SkillCatalogPort`、授权和 ToolRun session capability；运行期目录版本与内容读取直接调用已注入的 Skills port。
- App background bootstrap 复用 `ApplicationRuntime.services.skills`，不从 ToolsFacade 再取一份完整 bundle。
- `share_services()` 保留为 composition-time capability handoff；ToolsFacade 仍是 App/Agent adapter 执行工具目录投影和执行操作的窄 façade。测试可在测试装配代码使用共享 capability bundle。

## 影响

不改变授权、Tools/Agent/App capability 实现、生命周期、目录构建顺序、IPC、数据库、配置或用户行为。运行时 adapter 的依赖更窄，且不再为读取 Skills capability 克隆完整 `ToolServices`。这是内部 Rust 构造签名收敛，不保留旧 factory 入口。

## 验证

- 静态调用图确认 App adapter factories 不再调用 `share_services()`；Agent adapter 的运行方法不再调用它；App bootstrap 只读取已装配的 Skills port。
- 全 workspace formatting、check、tests 与严格 Clippy。
- ADR 索引检查。

## 替代方案

- 保留 `ToolsFacade` 参数并只在 factory 内读取一次：拒绝，因为 adapter 仍能任意取其它 capability，编译器无法表达其实际依赖。
- 每次运行期操作继续克隆 `ToolServices`：拒绝，因为这扩大了运行期可见面，并把构造 wiring 和业务调用混在一起。
- 删除 `ToolServices`：拒绝，因为组合根仍需一次性装配多个独立 consumer capability；bundle 适合作为 composition handoff，不适合作为运行期 service locator。

## 回滚

恢复 adapter factory 的 `ToolsFacade` 参数、Agent prompt adapter 的运行期 `share_services()` 调用和 App bootstrap 的 facade bundle 读取。无持久化或 wire 变化，无需重置数据。
