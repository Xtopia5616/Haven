# 1017：设置加载失败时禁止保存完整 fallback

## 状态

已采纳并实施（2026-10-12）。

## 背景

Settings 的更新提交完整设置快照。页面初始 `get_settings` 失败或返回无效根值时，此前仍会把 `settingsLoaded` 设为 true 并保存 UI fallback 作为基线；用户修改任意设置后即可把这份 fallback 提交，覆盖未成功读取的持久配置。`context_limits` 也使用 `Partial<ContextLimitsConfigInput>`，其本地默认对象漏掉了 Rust contract 中的 `scheduled_tool_runs_due_horizon_secs`，导致快照完整性没有编译期保障。

## 决定

- 只有初始 Settings 响应有效且页面成功应用后，才标记 `settingsLoaded` 并建立保存快照；请求失败、解析失败或无效响应不建立可保存状态。
- `saveSettings` 自身要求 loaded 状态和已建立的快照，作为 UI 保存入口的第二道保护。
- `contextLimits` 使用 `Required<ContextLimitsConfigInput>`，fallback 补齐 `scheduled_tool_runs_due_horizon_secs` 的现行 365 天默认值；新增输入字段缺少 fallback 时由 TypeScript 检查发现。

## 影响

失败或无效的初始读取后，用户仍可看到页面但不能把 fallback 整表提交。有效 Settings 的读写形状不变；数据库、配置文件 schema 与 IPC DTO 均未更改，无需重置数据。

## 替代方案

- 读取失败时仍允许保存页面可见字段：拒绝；后端目前接收完整设置并替换整个 `context_limits`，无法安全地区分用户未修改字段。
- 仅补一个遗漏字段而保留失败时可保存：拒绝；其它自定义配置仍可能被 fallback 覆盖。
- 将整表保存立即改为局部 patch：暂缓；这会改变 Settings 写契约，需单独定义各字段缺省、并发更新与撤销语义。

## 验证

- SettingsView 回归覆盖初始 command reject 与无效 `null` 响应；尝试修改后不得显示保存入口或调用 `update_settings`。
- fallback 保存回归确认 `scheduled_tool_runs_due_horizon_secs` 存在且取当前默认值。
- 运行 Settings contract/View 测试、完整 UI 测试及 Svelte 类型检查。

## 回滚

恢复旧行为会重新允许加载失败后的完整 fallback 覆盖现有配置；不涉及数据迁移或重置。
