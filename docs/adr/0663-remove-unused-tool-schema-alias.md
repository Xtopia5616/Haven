# ADR 0663：删除未使用的 ToolSchema UI alias

## 背景

`ui/src/lib/contracts/tools.ts` 导出了 `ToolSchema = unknown` 并以注释说明工具 JSON schema。全仓只有这处声明，没有消费者；alias 不验证 schema，也没有比 `unknown` 增加的领域或生命周期语义。生成的工具响应契约已由 `GeneratedToolManifest` 等 DTO 独立拥有 `input_schema`。

## 决定

- 删除未使用的 `ToolSchema` 导出与注释。
- 保留生成 DTO 的 `input_schema` 字段与其宽松类型；不引入新的 schema 校验层。
- 无运行时、IPC、配置或持久化行为变化，无需重置。

## 验证

- `corepack pnpm run check`
- Prettier 对改动文件的格式检查
- `scripts/check-adr-index.ps1`

## 回滚与重置

恢复导出即可回滚，不涉及用户数据或缓存。
