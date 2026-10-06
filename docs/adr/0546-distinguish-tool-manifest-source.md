# ADR 0546：区分开放的 ToolManifestSource 与 UI ToolSource 分类

## 状态

已采纳并实施；UI 类型检查、测试与构建通过。

## 背景

`ui/src/lib/toolIdentity.ts` 的 `ToolSource` 是 UI 将工具名称归一后的闭合集合：`builtin`、`skill`、`mcp`。`ui/src/lib/toolManifest.ts` 也定义了 `ToolSource`，但它描述后端 manifest 的 source token；解析器只要求字符串非空，并保留未知值，以便后端新增来源时旧 UI 仍能读到 manifest。后者写成 `'builtin' | 'skill' | 'mcp' | string`，在 TypeScript 中等价于开放的 `string`，所以同名类型实际具有不同契约。

## 决定

1. 将 manifest 中开放的 source token 命名为 `ToolManifestSource`，包括 identity source、presentation represented source 与 accessor 返回值。
2. 保留 `toolIdentity.ts` 中闭合、归一后的 `ToolSource`。
3. 在命名规范中记录：开放 wire 值与闭合内部分类使用能体现契约角色的不同名字。

## 替代方案

- 把 parser 限制为当前三种值：拒绝。这样会改变未知来源值的接收行为，削弱前端的向前兼容性。
- 把展示分类改成任意字符串：拒绝。标签和分类逻辑只定义了三种已知类别，开放类型会掩盖运行期需要归一的边界。
- 把 manifest 类型挪入展示 helper：拒绝。manifest parser 是 IPC 边界 owner，展示分类是 UI 内部呈现概念。

## 影响与验证

- 仅更改 TypeScript 类型名；IPC payload、parser 接受范围、未知来源保留行为及 tool card 分类不变。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

将 `ToolManifestSource` 恢复为原 `ToolSource` 名称，并移除此 ADR 和命名/路线图记录。无需 IPC、配置、数据库或用户数据迁移。
