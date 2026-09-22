# ADR-0203：删除 UI 全局 stores Facade，按领域收口状态入口

- Status: Accepted
- Date: 2026-09-22
- Owners: Haven maintainers

## Context

`ui/src/lib/stores.ts` 同时承载会话错误缓存、工具输出预览、媒体计划、后台/定时任务、
通知、resume intent、录音 overlay、模型状态和消息构造。它表面上是一个通用 store 模块，
实际上把十个不同生命周期和副作用边界聚合成了一个 UI Facade：调用方无法从 import 判断
依赖的是会话状态、任务状态还是通知副作用，也容易继续向其中添加无关状态。

## Decision

1. 删除 `stores.ts`，不保留 re-export 兼容层。
2. 按领域将状态和副作用直接放入 `actionStore`、`mediaPlanStore`、`notificationStore`、
   `sessionErrorStore`、`sessionIntentStore`、`toolOutputPreviewStore` 和
   `runtimeStateStore`。
3. 将纯 helper 放入 `messageFactory` 与 `mediaData`；已有的 `messageFormat` 直接作为时间
   格式化的唯一入口，不再经由状态模块转发。
4. `SessionReducer` 仍是会话 transcript/runtime 的唯一权威；本次拆分只移动现有投影和
   UI cache，不新增第二套会话状态机，也不改变 IPC、事件或持久化契约。
5. `ResumeTarget`、录音 overlay 和模型状态补充显式 TypeScript 类型，防止模块拆分后重新
   退化为 `any` 或过窄的推断类型。

## Alternatives

- 保留 `stores.ts` 作为统一入口：拒绝，会延长内部迁移窗口并继续掩盖真实依赖方向。
- 将所有状态拆到一个 `uiStore`：拒绝，只是把文件名改掉，仍然保留同一个跨领域 Facade。
- 将这些状态全部并入 `SessionReducer`：拒绝，通知、录音和任务生命周期不属于会话 transcript
  authority；工具输出预览也必须保持在热路径之外。

## Consequences

- UI 调用方通过模块名表达领域依赖，后续可独立测试或替换各状态边界。
- 旧的内部 `stores.ts` import 路径被破坏性删除；当前仓库调用方已全部迁移。
- 行为、store key、事件处理、IPC 和用户数据均不变，不需要数据库或配置重置。
- `stores.test.ts` 暂时保留为历史测试文件名，但测试直接从领域模块导入；后续可按领域拆分
  测试文件，不得重新引入聚合 Facade。

## Verification

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
rg "stores\\.ts|from ['\"]\\./stores|from ['\"]\\$lib/stores" ui/src
```

预期结果：类型检查无诊断，UI 全量测试通过，源码中不存在旧 `stores.ts` 导入。
