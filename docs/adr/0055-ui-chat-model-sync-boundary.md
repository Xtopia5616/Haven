# ADR 0055：UI 聊天模型同步边界

## 背景

聊天路由页同时承载会话状态、事件订阅、消息交互、模型菜单和默认 provider 的
模型发现。默认模型设置同步还包含请求合并、provider 切换后的过期响应丢弃、
联网搜索能力归一化和刷新代次控制，继续放在 `+page.svelte` 会扩大路由热点。

## 决定

- 新增 `ui/src/lib/chatModelSync.ts`，集中承载默认模型发现缓存、设置投影、
  provider 能力归一化、联网搜索降级和后台刷新代次。
- `+page.svelte` 继续拥有 Svelte 响应式状态和模型菜单交互，通过 setter 回调
  接收同步结果；不改变 Tauri 命令、事件、模型配置或 UI 行为契约。
- 保持 provider 切换时丢弃旧请求、并发挂载共享 in-flight 请求、Gemini 搜索
  模式归一化和组件销毁后的回调保护。

## 替代方案

- 继续把发现缓存和配置同步留在 `+page.svelte`：会让路由编排与 provider 状态
  策略继续混合，拒绝。
- 把整个模型菜单交互一起下沉：会把 Svelte 响应式状态和通知生命周期带入通用
  模块，边界过大，拒绝。

## 影响

这是 UI 内部模块重组；默认模型加载、切换、联网搜索与设置保存行为不变，模型
发现/同步逻辑从路由热点移出，Tauri IPC 和持久化契约不变。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
```

## 回滚

将 `chatModelSync.ts` 中的缓存和同步函数重新并回 `+page.svelte`，恢复原调用
位置；不需要数据迁移。
