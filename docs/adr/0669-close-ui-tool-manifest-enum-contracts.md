# ADR 0669：统一 UI Tool manifest 枚举契约

## 背景

Rust generated IPC contract 已为 Tool manifest 声明 `ToolSource`、`ToolCatalogGroup` 与 `RiskLevel` 及其 values。UI 的 `ToolManifestView` 却把 source 扩成 `'builtin' | 'skill' | 'mcp' | (string & {})`，把 catalog group 和 risk 也定义为普通字符串；runtime parser 只验证它们非空。未知 represented source 会在 UI badge 分类中落入 `builtin`，未知 catalog group 使用原值作为类别标签，未知 risk 也能进入设置视图。

目前可合法传输的枚举只有 Rust generated contract 中的成员。保留任意字符串既复制契约，又会把未识别值伪装成已知类别。

## 决定

- `ToolManifestView` 的 source、represented source、catalog group 与 risk level 直接引用 generated `ToolSource`、`ToolCatalogGroup` 与 `RiskLevel`。
- `parseToolManifest` 使用 generated values 校验 unknown runtime payload；未知成员使该 manifest 无效并被现有 catalog parser 丢弃。
- builtin Tool settings view 的 category、risk 和 family group name 使用生成类型；移除未知 catalog group 的 fallback label/order。
- 已知 source 直接投影到 UI badge；没有 manifest 时保留现有 `mcp__`、`skill__` 名称识别路径。
- 外部 LLM/MCP provider 的互操作字段和值域不变；本决定只针对 Rust→UI Tool manifest 契约。无持久化或配置影响，无需重置。

## 验证

- `corepack pnpm run check`
- Prettier 对改动 TypeScript 文件的格式检查
- `scripts/check-adr-index.ps1`

## 回滚与重置

恢复开放字符串和旧 parser 即可回滚；没有数据库、配置或用户数据变化，无需重置。
