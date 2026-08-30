# ADR 0043：UI 会话用量展示边界

## 背景

会话用量 store 已独立，但聊天路由页仍直接计算每步 LLM 调用聚合、缓存命中率和
token tooltip，导致持久化用量数据与展示格式化职责重新耦合。

## 决定

- 新增 `ui/src/lib/sessionUsagePresentation.ts`，集中承载每步用量聚合、缓存命中率、
  token tooltip 和对应的展示类型。
- 路由页保留响应式 store 同步与 context budget 派生，只通过轻量适配函数把当前
  `llmUsage` 传给展示模块；工具栏继续通过 props 接收展示回调。
- 保持多次调用合并、inclusive/exclusive cache accounting、恢复态/估算态、费用和
  上下文百分比的现有显示语义；不改变后端 usage DTO 或持久化行为。

## 替代方案

- 继续把格式化逻辑留在路由页：会延续状态与展示职责混合，拒绝。
- 把展示函数放入 store 模块：会让状态模块承担 UI 文案，拒绝。
- 修改 usage 数据契约以适配展示：没有必要且扩大跨层风险，暂不采用。

## 影响

这是 UI 内部纯计算边界拆分。token 卡片、工具气泡和 tooltip 的展示保持不变，不需要
数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有聊天与 store 测试覆盖步骤用量、恢复态及缓存显示路径。

## 回滚与重置

代码回滚时删除 `sessionUsagePresentation.ts`，恢复 `+page.svelte` 中的聚合与
tooltip 实现；本次不改变持久化数据或配置，不需要用户重置。
