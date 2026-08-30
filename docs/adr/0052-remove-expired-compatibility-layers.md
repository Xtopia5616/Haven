# ADR 0052：删除已到期的兼容层

## 背景

Haven 是测试阶段的单机应用，发布政策允许对旧数据库和 UI 内部模块进行破坏性调整。
当前代码仍保留两条没有继续价值的兼容路径：`sessions.react_state` 接受压缩前的
纯文本快照，以及 `stores.ts` 对已经拆出的会话消息/用量模块提供旧导出路径。这些路径
掩盖了版本边界，也让新数据契约无法被严格验证。

## 决定

- `react_state` 只接受 `save_react_state` 写入的 gzip BLOB；纯文本行或非 gzip BLOB
  视为不兼容并硬失败，用户按发布说明删除数据根目录后重建。
- `ui/src/lib/sessionMessages.ts` 和 `ui/src/lib/sessionUsage.ts` 成为唯一导入入口；
  删除 `stores.ts` 的 re-export，并把仓库内旧导入全部迁移到对应模块。
- TypeScript 6 依赖保留，等待上游公共 API 与 TypeScript 7 原生编译器完全兼容后再单独处理。

## 替代方案

- 继续读取纯文本快照：会在压缩契约外产生第二种持久化格式，拒绝。
- 继续保留 `stores.ts` re-export：会延长已完成模块拆分的迁移窗口并阻止依赖方向收口，拒绝。
- 为旧快照增加在线迁移：测试版不承诺旧快照兼容，且迁移无法覆盖 schema/事件版本漂移，拒绝。

## 影响

这是一次有意的兼容性破坏。旧未压缩 snapshot 所在的数据根目录需要完整重置；使用旧
`stores.ts` 导入路径的仓库内代码已在本次变更中迁移，外部 UI 复用代码不属于当前发布承诺。
新的 snapshot 写入与读取格式、Tauri IPC、模型配置和运行时行为不变。

## 验证

```text
cargo test --locked -p haven-memory -- react_state
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
rg "from .*stores" ui/src
```

其中 memory 测试验证压缩快照正常 round-trip、旧纯文本快照返回 reset 错误；导入检查确认
会话消息/用量没有回到 `stores.ts`。

## 回滚与重置

代码回滚需恢复 `stores.ts` re-export 和旧 snapshot 读取分支；如果新版本已经写入压缩快照，
回滚前必须保留完整数据根目录备份。按当前发布策略，遇到不兼容旧数据时删除
`%APPDATA%\\haven`（非 Windows 为 `~/.local/share/haven`）后重新配置。
