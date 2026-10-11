# 1003：收敛 Skills command response 校验与 renderer view

## 状态

已采纳并实施。

## 背景

`list_skills` 的 renderer 直接消费生成的 Rust snake_case `SkillInfo`，没有校验完整字段或重复名称；`execute_skill` 则把未知 invoke 返回值直接当作 `SkillExecutionResponse`。这让跨层字段风格不一致，也没有在 command adapter 检查固定外层契约。

## 决定

- 新增 `contracts/skills.ts` 作为 Skills command response 边界。
- `list_skills` 校验每个 `SkillInfo` 的必需字段、nullable 诊断字段和列表内 name 唯一性，然后映射为 camelCase `SkillInfoView`；Rust wire 字段 `root` 在 UI 中命名为 `rootPath`。
- `execute_skill` 校验固定 envelope 的 `success`、必需 `output` 和 nullable `error`，动态 JSON `output` 原样保留，并继续复用生成的 `SkillExecutionResponse` 类型。
- Skill 名按非空身份处理，不在 UI 复制 Rust 的路径安全规则；那些规则由 Skills crate owner 维护。
- Rust command、生成 wire DTO、执行授权和动态 Skill output shape 保持不变。

## 影响

不完整、类型错误或包含重复 name 的 Skills 列表会在进入 UI state 前失败；Skills 卡片只消费 camelCase view。执行 output 保留原始 JSON 值，不假设其领域 shape。此切片不更改持久化 schema 或 Rust IPC payload。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- 对本次改动的 UI 文件运行 Prettier 检查。
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 回滚

无需数据迁移。若移除 view/parser，必须同时恢复完整 Skills DTO 的校验以及 `execute_skill` 固定 envelope 校验，避免重新将 Rust snake_case 字段和未经验证的 invoke 结果交给 renderer。
