# Haven 竞态条件审查报告

> 审查时间: 2026-07-29 | 审查范围: 全部 Rust crates + Svelte 5 前端

---

## 修复 Checklist

### 🔴 Critical

- [x] **C1** — 锁序死锁: `update_task_status` (`tasks`→`running_tasks`) vs `unmark_running` (`running_tasks`→`tasks`)
  > `crates/task/src/lib.rs:509-542` vs `crates/task/src/lib.rs:316-332`
- [x] **C2** — Rollback 状态覆盖: 并发工具执行在 snapshot 恢复后写入步骤记录
  > `crates/agent/src/lib.rs:195-327`, `crates/task/src/lib.rs:618-707`, `crates/agent/src/react.rs:459-597`
- [x] **C3** — 录音覆盖层状态过时: `vad_status` handler 读取过时的 `isRecording`
  > `ui/src/routes/+layout.svelte:156-161`

### 🟠 High

- [x] **H1** — `end_task` 取消 default token；真实 token 永不被取消
  > `crates/task/src/lib.rs:393-394, 544-551, 251-254`
- [x] **H2** — Lost wakeup: `update_task_status` 仅对已存在的 Notify 发送通知
  > `crates/task/src/lib.rs:525-527, 502-507`
- [x] **H3** — MCP `call_tool` 持有 inner lock 等待 30s 网络 I/O → 阻塞 reconnect
  > `crates/tools/src/mcp/mod.rs:426-441, 337-349`
- [x] **H4** — Input pipeline 取消+启动交替: 窃取新录音的 token/rx
  > `crates/input/src/lib.rs:626-653, 454-511`
- [x] **H5** — 前端 `loadTasks()` 并发无保护 (过时响应覆盖新响应)
  > `ui/src/routes/+page.svelte:403-422`
- [x] **H6** — 前端 `activeTaskId` 竞争: `endTask`/`newTask` 与事件驱动 `loadTasks()` 复活已结束任务
  > `ui/src/routes/+page.svelte:109-123, 403-422`
- [x] **H7** — DB: `add_message_with_window` 缺少事务 (INSERT + DELETE 非原子)
  > `crates/memory/src/repositories/messages.rs:39-68`
- [x] **H8** — DB: `increment_counter` 读-改-写竞态 (丢失并发增量)
  > `crates/memory/src/repositories/preferences.rs:86-109`
- [x] **H9** — DB 查询缓存覆写: 并发 invalidate 后写入过时缓存
  > `crates/memory/src/db.rs`, `messages.rs:70-99`, `facts.rs:46-72`
- [x] **H10** — 任务复活: `supplement_task`/`process_input` 重新加载已结束任务并设为 Pending
  > `crates/agent/src/lib.rs:91-126, 569-640`
- [x] **H11** — SSL 读取任务泄漏: HTTP streaming spawned reader 无取消路径
  > `crates/llm/src/client.rs:542-597`

### 🟡 Medium

- [ ] **M1** — TOCTOU: dispatcher spawn handler 时任务可能已被 `end_task` 移除 (H1 修复后风险降低)
  > `task/lib.rs:230-310`
- [x] **M2** — Critical 优先级预占未释放 semaphore permit 或取消 token (Critical 优先级未被使用)
  > `task/lib.rs:193-203`
- [ ] **M3** — `process_input` TOCTOU: 状态读取与操作之间释放了锁 (H10 修复后风险降低)
  > `agent/lib.rs:579-618`
- [x] **M4** — 取消检测在 `FuturesUnordered` drain 期间不生效
  > `react.rs:557`
- [x] **M5** — `current_run_id` AtomicU64 在并发任务间共享产生 `run_id` 冲突
  > `react.rs:60-70`, `lib.rs:342-343`
- [ ] **M6** — `update_settings` 用 `std::sync::RwLock` 热替换 router 阻塞 tokio worker (临界区极短，实际风险低)
  > `react.rs:52-53`, `commands.rs:1225`
- [x] **M7** — Tool registry rebuild 与 Agent 并发工具查找竞态 (替换间隙工具消失)
  > `tool.rs:175-177`, `tools/lib.rs:137`
- [ ] **M8** — LlmRouter 断路器 TOCTOU: 并发 success 过早关闭 breaker
  > `router.rs:202-212, 396-487`
- [x] **M9** — MCP reconnect 中 cancel token 竞态 (取消旧 token → 新 token 创建间隙)
  > `mcp/mod.rs:486-488`
- [x] **M10** — Safety gateway `min_risk_level` 与 `session_trusted_levels` 分步读取不一致
  > `tool.rs:219-227`
- [x] **M11** — 确认对话框覆写: 两个并发确认到达时第一个被丢弃
  > `ui/+page.svelte:374`
- [x] **M12** — History `loadHistory`/`loadMore` 并发无锁保护
  > `ui/history/+page.svelte:75`
- [ ] **M13** — DB migration 逻辑在 connection mutex 初始化前运行 (仅启动时运行，实际风险低)
  > `db.rs:30`, `migrations.rs:82`
- [ ] **M14** — Messages 排序仅依赖 `created_at` (实际为纳秒精度 RFC3339，碰撞极不可能)
  > `messages.rs:42, 49-54`
- [ ] **M15** — `modelStateTimer` 模块级共享可变状态 (已部分缓解)
  > `ui/stores.js:186`
- [x] **M16** — `delete_task`/`clear_tasks` 多步 DB 操作无事务包裹
  > `tasks.rs:205-230`

### 🟢 Low

- [ ] **L1** — `unmark_running` 与 `update_task_status` 双重清理 (无害)
  > `task/lib.rs:316-332, 532-538`
- [ ] **L2** — Rollback 轮询超时 5s 不足以等待长 LLM 调用 (C2 修复后已有日志告警)
  > `agent/lib.rs:212-218`
- [ ] **L3** — `end_task` 从 DB 读 title 而非在移除前从内存读
  > `commands.rs:237-252`
- [ ] **L4** — `resolve_confirmation` 的 `trust_risk_level` 与 `confirm_step` 非原子
  > `commands.rs:337-350`
- [ ] **L5** — `update_settings` 中 `task_notify` 双重锁获取 (C1 修复后已重构)
  > `task/lib.rs:525, 536`
- [ ] **L6** — Registry TOCTOU: `rebuild` 两次 write lock 之间短暂不一致 (M7 修复后已消除)
  > `tool.rs:175-177`
- [ ] **L7** — Audio capture `failed` AtomicBool 从未在 `recording_loop` 中检查
  > `input/lib.rs:233, 553`
- [ ] **L8** — CPAL audio callback 与 drain 之间 ring buffer 锁竞争
  > `input/lib.rs:95-103, 244-251`
- [x] **L9** — History `task:title-updated` 直接突变对象 (可能不触发 Svelte 响应)
  > `ui/history/+page.svelte:52`
- [ ] **L10** — `addNotification` ID 碰撞理论可能
  > `ui/stores.js:30`
- [ ] **L11** — ChatBubble `onMount` async import 无 abort/mounted 检查
  > `ui/ChatBubble.svelte:25`
- [ ] **L12** — Settings 页面 `onDestroy` 可能在 `onMount` 完成前恢复 accent
  > `ui/settings/+page.svelte:60`

---

## 修复优先级

| 优先级 | 问题编号 | 影响 |
|--------|---------|------|
| P0 | C1 | 死锁 — tokio worker 永久阻塞，任务管线停止 |
| P1 | H1, H2 | `end_task` 无效 + handler 永久 stuck |
| P1 | C2 | Rollback 后数据损坏 |
| P1 | C3 | 录音 UI 状态错误 |
| P2 | H3-H6 | 功能正确性问题 |
| P2 | H7-H9 | DB 数据一致性 |
| P3 | H10, H11 | 功能异常 |
| P4 | M1-M16 | 边界条件下的非确定性行为 |
| P5 | L1-L12 | 次要问题或已部分缓解 |

---

## 详细分析

### C1. 锁序死锁: `update_task_status` 与 `unmark_running`

**文件**: `crates/task/src/lib.rs:509-542, 316-332`

`update_task_status` 持锁顺序: `tasks` (L517) → `task_notify` (L525) → `running_tasks` (L533)，全程持有 `tasks` 不解锁。

```rust
// update_task_status
let mut tasks = self.tasks.lock().await;         // L517
// ... 仍持有 tasks ...
if let Some(notify) = self.task_notify.lock().await.get(task_id) { // L525
    notify.notify_waiters();
}
if is_terminal {
    self.running_tasks.lock().await.remove(task_id);  // L533 ← 还持有 tasks!
    // ...
    tasks.remove(pos);  // L538
}
```

`unmark_running` 持锁顺序相反:

```rust
// unmark_running
self.running_tasks.lock().await.remove(task_id);  // L317
self.task_permits.lock().await.remove(task_id);   // L318
self.task_cancellations.lock().await.remove(task_id); // L319
let mut tasks = self.tasks.lock().await;          // L320 ← 还持有 running_tasks!
```

**竞态场景**: 当 `supplement_task`/`rollback_task` 调用 `update_task_status(Y, Pending)` 且 dispatcher 同时调用 `unmark_running(Y)` 同一任务时:
1. `update_task_status` 获取 `tasks` 锁
2. `unmark_running` 获取 `running_tasks` 锁
3. `update_task_status` 阻塞等待 `running_tasks`
4. `unmark_running` 阻塞等待 `tasks`
5. 死锁 — 两个 tokio task 永久 stall

**修复**: 将 `update_task_status` L532-539 的 `tasks` 锁提前释放，仿照 `end_task` L407 的 `drop(tasks)` 模式:

```rust
let mut tasks = self.tasks.lock().await;
// ... 状态更新 ...
let should_remove = is_terminal;
if is_terminal {
    // 先计算 pos
    let pos = tasks.iter().position(|t| t.id == task_id);
    drop(tasks);  // ← 释放 tasks 锁
    self.running_tasks.lock().await.remove(task_id);
    self.task_permits.lock().await.remove(task_id);
    self.task_cancellations.lock().await.remove(task_id);
    self.task_notify.lock().await.remove(task_id);
    self.tools.unregister_task(task_id).await;
    if let Some(pos) = pos {
        self.tasks.lock().await.remove(pos);
    }
}
```

---

### C2. Rollback 状态覆盖

**文件**: `crates/agent/src/lib.rs:195-327`, `crates/task/src/lib.rs:618-707`, `crates/agent/src/react.rs:459-597`

`rollback_task` 流程:
1. L207: `cancel.cancel()` — 非阻塞信号
2. L213-218: poll 循环等待 `running_tasks_list()` 清空（最多 5s）
3. 恢复 snapshot 到 DB

但 ReAct loop 仅在 step 边界检查 `cancel.is_cancelled()` (`react.rs:119`)，不在工具执行期间检查。

**竞态场景**: 若工具调用正在执行:
1. `cancel.cancel()` 发送信号
2. `execute_step` 中的工具调用已完成（信号未传播到内部）
3. `execute_step` 写入 `create_action_step` + `complete_action_step` (task/lib.rs:693-705)
4. Rollback 恢复 snapshot 到同一 task
5. 新 snapshot 与刚写入的步骤记录冲突 → 孤儿步骤记录 + 内存状态污染

**修复**: 取消后等待 `running_tasks_list` 清空**之后再** re-read snapshot 再保存。或在 `execute_step` 中 DB 写前检查 task 是否仍在 `running_tasks` 中/当前 run_id 是否仍然有效。

---

### C3. 录音覆盖层状态过时

**文件**: `ui/src/lib/+layout.svelte:156-161`

```javascript
// subscribe 获取 overlay state (异步)
recordingOverlay.subscribe((v) => { overlay = v; });

// vad_status handler
if (overlay.isRecording) {  // ← 可能已过时
    setOverlay({ vadState: data.state || 'silent' });
}
```

**竞态场景**:
1. `recording:stopped` 事件触发 → `isRecording = false`
2. subscribe 回调尚未执行（异步）
3. `recording:vad_status` 事件到达
4. handler 读取 `overlay.isRecording === true` → 重新设置 overlay state → 已停止的录音指示器重新显示

**修复**: 使用 `get(recordingOverlay)` 直接同步读取 store 当前值:
```js
import { get } from 'svelte/store';
if (get(recordingOverlay).isRecording) {
    setOverlay({ vadState: data.state || 'silent' });
}
```

---

### H1. `end_task` 取消 default token

**文件**: `crates/task/src/lib.rs:393-394, 544-551, 251-254`

时序:
1. Dispatcher: `take_next_pending` 返回 `task_id` (L230)
2. 用户: 调用 `end_task`
3. `end_task` → `cancellation_token(task_id)` → `task_cancellations.get()` 返回 `None` → `unwrap_or_default()` → 返回 `CancellationToken::default()`
4. `cancel.cancel()` → no-op (default token 无观察者)
5. Dispatcher: `cancels.insert(task_id.clone(), CancellationToken::new())` (L253) — 创建新 token，永不被取消
6. Handler 检查 `cancel.is_cancelled()` → `false` → 继续执行

**修复**:
```rust
pub async fn end_task(&self, task_id: &str) -> anyhow::Result<TaskStatus> {
    // 确保 token 存在并取消
    {
        let mut cancels = self.task_cancellations.lock().await;
        let token = cancels.entry(task_id.to_string())
            .or_insert_with(|| CancellationToken::new());
        token.cancel();
    }
    // ... 剩余的 end_task 逻辑 ...
}
```

---

### H2. Lost wakeup: Notify 通知丢失

**文件**: `crates/task/src/lib.rs:525-527, 502-507`

`update_task_status` 用 `get()` 获取 Notify:
```rust
if let Some(notify) = self.task_notify.lock().await.get(task_id) {
    notify.notify_waiters();
}
```

`status_notifier` 用 `entry().or_insert_with()`:
```rust
let notify = self.task_notify.lock().await
    .entry(task_id.to_string())
    .or_insert_with(|| Arc::new(Notify::new()))
    .clone();
```

**竞态场景**:
1. ReAct loop: `get_task_state()` → Paused → 保存 snapshot
2. ReAct loop: 尚未调用 `status_notifier`
3. `supplement_task`: `update_task_status(Pending)` → `get()` 返回 `None` → 无通知
4. ReAct loop: `status_notifier(task_id)` → 创建新 Notify → `notified().await` → 永久阻塞

**修复**: `update_task_status` 改用 `or_insert_with`:
```rust
let notify = self.task_notify.lock().await
    .entry(task_id.to_string())
    .or_insert_with(|| Arc::new(Notify::new()))
    .clone();
notify.notify_waiters();
```

---

### H3. MCP `call_tool` 持有 inner lock 跨越网络 I/O

**文件**: `crates/tools/src/mcp/mod.rs:426-441, 337-349`

```rust
let mut guard = self.inner.lock().await;  // L426
let result = inner.request(id, "tools/call", ...).await?;  // L432 — 最长 30s
```

`reconnect` 调用 `self.shutdown()` 也需要 `self.inner.lock().await` (L338)。若 MCP server 无响应，`call_tool` 持锁 30s，期间所有 reconnect/monitor 无法执行。

**修复**: 在持锁范围内 clone 需要的状态，释放锁后再执行网络调用。

---

### H4. Input pipeline 取消/启动交替

**文件**: `crates/input/src/lib.rs:626-653, 454-511`

`cancel_recording` 操作两个不连续步骤:
1. 设置 state → Pending，释放锁
2. `token.cancel()` + `rx.take()`

在这两步之间，新 `start_recording` 可以交插入创建新 token 和 result_rx → 然后 `cancel_recording` 取走新录音的 rx。

**修复**: 使用 generation counter 验证 token 和 rx 属于当前录音会话。

---

### H5. 前端 `loadTasks()` 并发无保护

**文件**: `ui/src/routes/+page.svelte:403-422`

7 个事件 handler 调用 `loadTasks()`。连续事件触发时，后发起的请求可能先完成，随后被先发起的过时响应覆盖。

**修复**: 使用 AbortController:
```js
let loadTasksController = null;
async function loadTasks() {
    if (loadTasksController) loadTasksController.abort();
    const controller = new AbortController();
    loadTasksController = controller;
    try {
        const result = await invoke('get_tasks');
        if (controller.signal.aborted) return;
        // ...
    } finally {
        if (loadTasksController === controller) loadTasksController = null;
    }
}
```

---

### H6. 前端 `activeTaskId` 竞争复活任务

**文件**: `ui/src/routes/+page.svelte:109-123, 403-422`

`endTask()` 设置 `activeTaskId = null` 后 fire-and-forget `invoke('end_task')`。若后端事件在此期间触发 `loadTasks()`，`loadTasks()` L409-416 发现任务在列表中 → 重新赋值 `activeTaskId`。

**修复**: `await invoke('end_task')` 后再清状态，添加 suppress 标志。

---

### H7. DB: `add_message_with_window` 缺少事务

**文件**: `crates/memory/src/repositories/messages.rs:39-68`

INSERT + DELETE 作为两个独立语句执行。并发调用可能使第二个 DELETE 移除第一个刚插入的消息。

**修复**: 包裹在 `BEGIN IMMEDIATE; INSERT...; DELETE...; COMMIT;` 中。

---

### H8. DB: `increment_counter` 读-改-写竞态

**文件**: `crates/memory/src/repositories/preferences.rs:86-109`

```rust
let current = conn.query_row("SELECT value FROM preferences WHERE key=?1", ...);
conn.execute("INSERT INTO preferences ... ON CONFLICT DO UPDATE SET value=?2", ...);
```

并发增量为同一 key 时丢失计数。

**修复**: 使用 `UPDATE preferences SET value = CAST(value AS INTEGER) + ?1 WHERE key = ?2`，仅当影响行数为 0 时 fallback INSERT。

---

### H9. DB 查询缓存覆写

**文件**: `crates/memory/src/db.rs`, `messages.rs:70-99`, `facts.rs:46-72`

缓存未命中 → 查 DB → `cache_put`。在查 DB 和 `cache_put` 之间，另一个写入 invalidate 了缓存。然后 `cache_put` 用过时快照覆盖有效缓存。

**修复**: 使用 generation counter 或写前检查缓存仍为空。

---

### H10. 任务复活

**文件**: `crates/agent/src/lib.rs:91-126, 569-640`

`process_input` 读取状态后释放锁。若 `end_task` 并发移除任务，`add_supplement` 失败触发 `ensure_task_loaded` → 从 DB 重新加载 → 置为 Pending。

**修复**: `ensure_task_loaded` 后检查 DB 状态，若已 Completed/Error 则不转换为 Pending。

---

### H11. SSL 读取任务泄漏

**文件**: `crates/llm/src/client.rs:542-597`

`chat_stream_inner` 的 spawned reader task 仅在字节流结束时退出，永不检查取消 token。消费者取消后 reader task 继续读取整个响应体。

**修复**: 传入 CancellationToken 到 reader task，在循环中检查 `token.is_cancelled()`。

---

### Medium/Low 问题

详见上方 checklist 中的文件位置和简要描述。修复方式和上述 High 问题类似，可逐项问询详细信息。
