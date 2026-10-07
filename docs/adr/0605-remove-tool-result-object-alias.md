# ADR 0605：删除 UI Tool result parser 空泛 alias

## 状态

已采纳并实施。

## 背景

`toolResultParsing.ts` 私有 `ToolResultObject` 只是 `Record<string, any>` 的别名，仅用于 JSON object type guard 和 shell 分支局部变量；解析出的 data 对外一直以 `unknown` 表达，代码不读取任意属性。该 alias 没有增加字段约束或独立领域角色。

## 决定

1. 删除 `ToolResultObject`，type guard 直接将对象收窄为 `Record<string, unknown>`。
2. shell 分支局部变量使用相同的具名结构类型，保留 `null` 与解析失败语义。
3. renderer payload 仍以 `unknown` 暴露，调用方必须自行按 renderer contract 解释字段。

## 替代方案

- 保留别名并改成 `Record<string, unknown>`：拒绝，别名仍未增加 parser 专用约束或角色。
- 将解析结果整体改为开放记录：拒绝，稳定 renderer boundary 继续保留 `unknown`；该切片只约束 parser 判断“这是一个非数组对象”。

## 影响与验证

- 仅变更 UI 内部静态类型，不改 JSON parse 行为、tool kind 分类、custom renderer 选择或 IPC shape。
- 命名路线图 §5.7 继续保持 Active；组件、store、controller 与 event handler 命名仍待审计。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `ToolResultObject` alias 与 `any` 记录类型；无持久化或 wire 迁移。
