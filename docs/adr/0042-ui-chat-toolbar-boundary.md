# ADR 0042：UI 聊天工具栏边界

## 背景

聊天路由页除了会话事件与输入编排，还直接承载会话切换、token 概览、模型切换、
思考强度和内置联网搜索菜单的完整模板与样式，扩大了页面热点。

## 决定

- 新增 `SessionToolbar.svelte`，承载会话切换菜单、结束会话按钮和 token 概览。
- 新增 `ModelToolbar.svelte`，承载模型、思考强度和联网搜索菜单。
- `+page.svelte` 继续拥有状态、加载与 IPC 编排，通过显式 props 和回调把数据与
  操作交给两个展示组件；保持 `InputRouter` 的 `toolbarLeft`/`toolbarRight`
  snippet 契约不变。
- 保持并行会话状态标签、token 累计/上下文显示、模型/联网搜索选项与原有中文
  文案；不改变设置 IPC、会话生命周期或模型配置语义。

## 替代方案

- 继续把工具栏模板留在路由页：会延续页面热点，拒绝。
- 在工具栏组件内部重新读取 store 或调用 IPC：会形成第二个状态编排入口，拒绝。
- 修改 `InputRouter` 的 toolbar snippet 契约：没有必要且扩大输入边界风险，暂不采用。

## 影响

这是 UI 内部展示边界拆分。工具栏交互和状态来源保持不变，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有聊天页/输入路由测试覆盖 toolbar 回调链路和渲染兼容性。

## 回滚与重置

代码回滚时删除两个工具栏组件，恢复 `+page.svelte` 中的 toolbar snippets 与样式；
本次不改变持久化数据或配置，不需要用户重置。
