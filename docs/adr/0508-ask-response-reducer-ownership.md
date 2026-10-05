# ADR 0508：Ask 响应以 SessionReducer 为唯一来源

## 状态

已采纳；实现与 UI 门禁通过（2026-10-05）。

## 背景

`chatAskInteraction.ts` 将 `session/interaction-resolved` 的结果交给 `SessionReducer`，同时又在 `resolvedAskResponses` 保存同一份 answer/ignored response。提交批次时实际读取 controller map，而可见 Ask 卡片已由 `chatVisibleMessages.ts` 从 reducer interaction 投影响应。这形成两个可变副本，resume、terminal clear 和当前批次 submit 必须同步维护。

`clearAskAwaiting` 还分两次 dispatch：先结算 transcript Ask message，再删除同 session 的 Ask interactions。两次状态通知之间会短暂出现卡片已 settled、interaction 仍存在的中间状态。

`resolvedAskIds` 虽与 reducer 的 resolved status 有重叠，但它还表示本次 controller 批次成员并承担重复点击保护；历史 resolved requests 不应自动加入后续提交批次。

## 决定

1. SessionReducer 中匹配当前 session owner、kind=`ask`、status=`resolved` 的 interaction response 是已接受答案的唯一来源。删除 `resolvedAskResponses`；提交时仍按 transcript 顺序遍历 controller 当前批次 IDs，并从 reducer 读取对应响应。
2. 新增单一 reducer action `session/asks-settled`：一次 `SessionReducer.dispatch` 同时将对应 Ask 卡片设为 settled 并移除该 session 的 Ask interactions。它保留 resolved answer/ignored 投影，不触碰其他 session 或 interaction kind。现有 route-level lifecycle interaction clearing 语义保持不变。
3. 保留 controller-local 的 option selections 和 `resolvedAskIds`。前者是临时选择状态；后者限定当前提交批次并阻止重复 resolution。不得改为从所有历史 resolved interactions 推导提交成员。
4. 不抽取 Ask 组件、模块或 crate；不改 IPC、持久化 event、backend Ask 契约或 transcript 文案/顺序。

## 替代方案

- 删除 `resolvedAskIds` 并用 reducer 中所有 resolved Ask 构造答案：拒绝。历史答案会混入后续批次，也失去当前批次的重复点击保护。
- 保留 response map 作为快速缓存：拒绝。UI selector 已从 reducer 投影相同的响应，额外副本增加 resume/clear 同步面。
- 只把内部函数合并而保留两次 dispatch：拒绝。它不能消除订阅者可观察的半结算状态。
- 拆出 Ask UI 模块或组件：暂缓。现有 controller 的职责仍然单一，文件体量本身不构成稳定组件边界。

## 影响与验证

- 当前批次答案从 reducer 的 resolved interaction 读取；当前批次 ID 与选择仍由 controller 持有。
- ask settle 清理由一次 reducer 通知同时更新 transcript 和同 session Ask interaction。其他 interaction kind 的处理维持已有 route lifecycle 语义。
- 回归覆盖多 Ask 按 transcript 顺序提交、选项和额外输入/附件、重复点击、历史 resolved request 不进入新批次、resume 清理保留 settled 卡片响应，以及 session/kind 隔离和单次 reducer 通知。
- 无持久化或跨端契约变化，无需数据库或配置重置。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run` 通过。

## 回滚

回退至 controller 同时维护 reducer response 与 response map、以及两次 Ask 清理 dispatch 的实现即可；不涉及持久数据或 wire 契约。若回滚，保留已新增的测试语义并在路线图将候选恢复为 Next。
