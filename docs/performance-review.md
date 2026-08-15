# Haven 性能优化审查报告

> 审查日期: 2026-08-02
> 范围: `crates/` (Rust 后端, Tauri 2) + `ui/src/` (Svelte 5 前端)
> 目的: 记录可优化的位置与建议，作为后续优化任务的工作清单

---

## 总体结论

最大问题是**每次流式 chunk 触发完整的 markdown 重新解析 + DOM 重建**（前端），以及**同步 SQLite 在 Tokio runtime 内执行 + 全局 std Mutex 串行化**（后端）。两者叠加导致长会话与长任务的体验随时间明显退化。

---

## 🔴 高优先级（最大收益）

### 2. markdown-it + highlight.js 每个 ChatBubble 独立构造
**文件:** `ui/src/lib/ChatBubble.svelte:65-98`

每个气泡（用户/工具/推理）都动态 import 一次 + 创建自己的 `MarkdownIt` 实例 + 注册 8 个语言。改为**模块级单例懒加载**，并在 `role !== 'assistant' || msgType` 时直接跳过。

```js
// ui/src/lib/markdownRenderer.js
let md = null;
let loading = null;
export function getMarkdownRenderer() {
  if (md) return Promise.resolve(md);
  if (loading) return loading;
  loading = (async () => {
    const [{ default: MarkdownIt }, { default: hljs }, js, ts, bash, json, css, xml, rust, yaml] =
      await Promise.all([
        import('markdown-it'),
        import('highlight.js/lib/core'),
        import('highlight.js/lib/languages/javascript'),
        import('highlight.js/lib/languages/typescript'),
        import('highlight.js/lib/languages/bash'),
        import('highlight.js/lib/languages/json'),
        import('highlight.js/lib/languages/css'),
        import('highlight.js/lib/languages/xml'),
        import('highlight.js/lib/languages/rust'),
        import('highlight.js/lib/languages/yaml'),
      ]);
    hljs.registerLanguage('javascript', js);
    hljs.registerLanguage('typescript', ts);
    hljs.registerLanguage('bash', bash);
    hljs.registerLanguage('json', json);
    hljs.registerLanguage('css', css);
    hljs.registerLanguage('xml', xml);
    hljs.registerLanguage('rust', rust);
    hljs.registerLanguage('yaml', yaml);
    md = new MarkdownIt({ html: false, linkify: true, breaks: true, highlight: (str, lang) => {
      if (lang && hljs.getLanguage(lang)) {
        try { return hljs.highlight(str, { language: lang, ignoreIllegals: true }).value; }
        catch {}
      }
      return '';
    }});
    return md;
  })();
  return loading;
}
```

在 `ChatBubble.svelte`:
```js
onMount(async () => {
  if (role !== 'assistant' || msgType) return;
  md = await getMarkdownRenderer();
  if (mounted && !streaming) mdHtml = md.render(content || '');
});
```

---

### 3. 同步 SQLite 在 Tokio runtime 内执行 + 全局 std Mutex 串行化
**文件:** `crates/memory/db.rs:25-27`；调用点 `commands.rs:371,376,386,397,909,923,965,1220,1236,1281,1478`, `action/lib.rs:191,325,598,864`, `agent/src/react.rs:236,1164,1226`

每次 SQLite 操作（含 `synchronous=FULL` 的 fsync）都阻塞一个 worker 线程，并串行化所有 DB 任务；`action/lib.rs:317-332` 等路径还在持有 `tokio::Mutex` 期间做同步写入，把相关异步任务全部 stall 住。

**修复:**
- 给 `Database` 增加 async facade，所有调用包 `tokio::task::spawn_blocking`
- 考虑专用 DB worker thread + mpsc，方便后续合并批写入

```rust
// crates/memory/src/db.rs
impl Database {
    pub async fn list_actions_async(&self, limit: i64, offset: i64) -> Result<Vec<ActionRow>> {
        let conn = self.conn.clone(); // Arc<Mutex<Connection>>
        tokio::task::spawn_blocking(move || {
            let conn = conn.lock().unwrap();
            ActionRepo::list(&conn, limit, offset)
        }).await?
    }
}
```

或专用 worker:
```rust
enum DbCmd {
    ListActions { reply: oneshot::Sender<Result<Vec<ActionRow>>> },
    // ...
}

// db_worker.rs: tokio::task::spawn_blocking 持有 rx，串行处理
```

---

### 4. ReAct 循环每步做全量快照序列化 + O(n²) branch_point 累积
**文件:** `crates/agent/src/react.rs:741-748`, `:1149-1166`, `:1217-1237`

每步：`canonical.to_vec()` + `history.to_vec()` + `branch_points.clone()`（含所有之前步骤的全量对话副本）+ `serde_json::to_string(&snapshot)` + 同步 DB 写入。N 步后 `branch_points` 单字段就 O(N²)；snapshot 还会再序列化一次。

**修复:**
- rollback 不需要 per-step 完整快照，存 delta（`{ step_number, last_msg_at, since-prev-actions }`）
- `to_writer` 写入可复用 `Vec<u8>`
- DB 写入走 `spawn_blocking`
- 只在材料化变化（暂停/出错/取消）时落盘

```rust
// react.rs 内的 delta-snapshot
#[derive(Serialize, Deserialize)]
struct BranchPoint {
    step_number: u32,
    last_msg_at: i64,
    actions: Vec<serde_json::Value>, // 自上次 branch 以来新增的 tool actions
}

let mut buf = Vec::with_capacity(8 * 1024);
serde_json::to_writer(&mut buf, &delta)?;
tokio::task::spawn_blocking({
    let db = db.clone();
    let buf = buf;
    move || db.save_react_state(&buf)
}).await??;
```

---

### 5. 长对话无窗口化 / 无 visibility 隔离
**文件:** `ui/src/routes/+page.svelte:1141-1161`

几百个气泡全部留在 DOM，每个含完整 markdown `{@html}` 子树和大块 `<pre>` 工具观察，导致滚动卡顿 + 底部 auto-follow 强制 reflow。

**修复:**
- 先用 CSS `content-visibility: auto` 让离屏气泡跳过布局/绘制
- 再考虑按 200 条窗口化 + `IntersectionObserver` 顶部加载

`ui/src/app.css`:
```css
.bubble {
  content-visibility: auto;
  contain-intrinsic-size: auto 120px;
}
```

---

## 🟠 中优先级

### 6. `updateModelState('streaming')` 每 chunk 触发 store.notify
**文件:** `ui/src/lib/stores.js:212-227`

`set('streaming')` 即使值未变也通知所有订阅者（`+layout.svelte:48-51`、chat 页全部每 chunk 重跑一次）。加 `current !== state` 守卫，仅在真正变化时 `set`。

```js
import { get } from 'svelte/store';

export function updateModelState(state, opts = {}) {
  const current = get(modelStateStore);
  const changed = current !== state;
  if (modelStateTimer) clearTimeout(modelStateTimer);
  modelStateTimer = null;
  if (changed) modelStateStore.set(state);
  if (!changed && state !== 'ready') return;
  // ... existing timer logic
}
```

---

### 7. `messages` 不是 `$derived`，而是 state + effect 同步
**文件:** `ui/src/routes/+page.svelte:510-525`, `ui/src/lib/stores.js:64-69`

`updateActionMessages` 每 chunk 做 `{...m, [actionId]: fn(m[actionId]||[])}`（数组 + dict 双拷贝）。改为 `derived`，并在 `fn` 返回相同引用时跳过外层拷贝。

```js
// stores.js
export function updateActionMessages(actionId, fn) {
  actionMessagesStore.update((m) => {
    const next = fn(m[actionId] || []);
    return next === m[actionId] ? m : { ...m, [actionId]: next };
  });
}
```

```svelte
<!-- +page.svelte -->
<script>
  const messages = $derived(
    activeActionId
      ? (actionMessagesDict[activeActionId] || [])
      : (actionMessagesDict['_draft'] || [])
  );
</script>
```

---

### 8. Stream-rule 对累积文本每 chunk O(n²) 正则
**文件:** `crates/llm/src/router.rs:718-741`, `crates/llm/src/stream_rules.rs:81-93`

每 chunk 用每条规则正则扫整个累积文本 + 拿 tokio RwLock 读。

**修复:**
- `stream_rules` 配置为空的常见路径用 `OnceLock<bool>` 短路
- 否则只在 `text[prev_len..]` 增量上匹配（保留小重叠窗口处理跨 `\n`）

```rust
// router.rs
static RULES_ENABLED: OnceLock<bool> = OnceLock::new();
let rules_enabled = *RULES_ENABLED.get_or_init(|| !rules.is_empty());
if rules_enabled {
    let prev = self.last_text_len;
    let incremental = &text[prev..];
    check_stream_rules(&rules, incremental, &mut matched);
    self.last_text_len = text.len();
}
```

---

### 9. SSE reader 每行 `String::from_utf8_lossy` + `drain` + `trim().to_string()` 二次分配
**文件:** `crates/llm/src/openai.rs:549-565`

批多行 chunk 时 O(n²) 拷贝，每行至少 2 次分配。改为 `Vec<u8>` / `BytesMut` `split_off` 扫描，直接对字节切片 `serde_json::from_slice`。

```rust
let mut buf = BytesMut::with_capacity(8 * 1024);
while let Some(chunk) = stream.chunk().await? {
    buf.extend_from_slice(&chunk);
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let line = buf.split_to(pos);
        let _ = buf.split_to(1); // consume '\n'
        let line = &line[..];
        if line.starts_with(b"data: ") {
            let payload = &line[6..];
            if payload == b"[DONE]" { break; }
            let evt: StreamChunk = serde_json::from_slice(payload)?;
            // ship...
        }
    }
}
```

---

### 10. 流式 chunk 在聚合 channel 上 `try_send` 静默丢弃
**文件:** `crates/llm/src/router.rs:671-677, 713-715`

消费者慢时 LLM token 丢失（最终落库文本完好，但 UI 流式断字）。改成 `await send()`（聚合器已与 HTTP reader 解耦），或更大有界 channel + backpressure。

```rust
// router.rs
let (chunk_tx, chunk_rx) = mpsc::channel::<StreamChunk>(1024); // 扩大上限
// 消费者 .send() 改为 chunk_tx.send(evt).await 而不是 try_send
```

---

### 11. 缓存命中/写入各做一次 deep clone（含大附件 base64）
**文件:** `crates/memory/db.rs:64,109,143`；写入侧 `messages.rs:204`, `actions.rs:128`, `facts.rs:113`

`get_session_messages` 命中克隆整条 `Message`（含 10 MB 图），紧接着 put 路径再 clone 一次。缓存值改为 `Arc<Vec<Message>>`，put 路径直接 move 入缓存。

```rust
// db.rs
type CacheEntry = Arc<Vec<Message>>;

fn cache_get(&self, key: &str) -> Option<CacheEntry> {
    self.cache.lock().unwrap().get(key).map(|e| e.data.clone())
}

fn cache_put(&self, key: String, data: CacheEntry) {
    self.cache.lock().unwrap().put(key, CacheEntry::new(data));
}
```

---

### 12. 每消息独立事务 + `synchronous=FULL` fsync
**文件:** `crates/memory/src/messages.rs:106-138`, `crates/memory/db.rs:33`

每个 ReAct 步骤触发 4-5 次同步 DB 提交（每步约 5-7 次）。

**修复:**
- `PRAGMA synchronous=NORMAL`（WAL+NORMAL 对聊天历史足够）
- `persist_message` 改为批次 API，给 ReAct 循环一次性 commit 全部 step 消息

```rust
// db.rs 连接初始化
conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA temp_store = MEMORY;")?;
```

```rust
// messages.rs
pub fn persist_step_batch(&mut self, session_id: i64, msgs: Vec<NewMessage>) -> Result<Vec<i64>> {
    let tx = self.conn.transaction()?;
    let mut ids = Vec::with_capacity(msgs.len());
    for m in msgs {
        let id = insert_in_tx(&tx, session_id, m)?;
        ids.push(id);
    }
    prune_window_in_tx(&tx, session_id, WINDOW)?;
    tx.commit()?;
    Ok(ids)
}
```

---

### 13. CPAL 实时回调中每帧分配
**文件:** `crates/input/src/lib.rs:287-371`, `:147-167`, `:103-111`

I16 转换 `Vec<f32>`、`downmix` 第二次 `Vec`、`Resampler::process` 每次新 `out` Vec——实时线程分配易导致 dropouts。

**修复:**
- 前置 / 复用每个回调的 scratch buffer
- `Resampler::process` 改为 `&mut Vec<f32>` out-param
- 考虑 SPSC lock-free ring 替代 `Arc<Mutex>`

```rust
// resampler.rs
pub fn process_into(&mut self, input: &[f32], out: &mut Vec<f32>) {
    out.clear();
    // ...就地写入 out
}

// lib.rs cpal callback
struct CaptureScratch {
    mono: Vec<f32>,
    resampled: Vec<f32>,
}
let scratch = Arc::new(Mutex::new(CaptureScratch::default()));
// 在 callback 中 lock + 复用
```

---

### 14. VAD ONNX 模型每次录音重新解析 + 优化
**文件:** `crates/input/src/lib.rs:591-599`, `crates/input/src/vad.rs:32-40`

每次 `start_recording` 同步做 `into_optimized().into_runnable()`（100-300 ms），阻塞 Tokio runtime。VadEngine 改为 lazy-singleton，只在 recording 间 `reset()` 循环状态。

```rust
// vad.rs
pub struct VadEngine {
    model: tract_onnx::RunnableModel,
    state: Mutex<VadState>,
}

impl VadEngine {
    pub fn shared() -> &'static OnceLock<Arc<Self>> {
        static ENGINE: OnceLock<Arc<VadEngine>> = OnceLock::new();
        &ENGINE
    }
    pub fn get_or_init() -> Result<Arc<Self>> {
        Self::shared().get_or_init(|| -> Result<Arc<Self>> {
            let model = build_model()?; // include_bytes! + into_optimized + into_runnable
            Ok(Arc::new(Self { model, state: Mutex::new(VadState::default()) }))
        }).clone()
    }
    pub fn reset(&self) { *self.state.lock().unwrap() = VadState::default(); }
}
```

---

### 15. `dedup_facts` O(n²) 相关子查询 + 逐条 DELETE
**文件:** `crates/memory/src/facts.rs:197-227`

每次推理都跑一次。改为 window-function 单语句 + 复合索引 `(subject, predicate, object, tags)`。

```sql
-- migrations.sql
CREATE INDEX IF NOT EXISTS idx_facts_dedup
  ON facts(subject, predicate, object, tags);

-- dedup_facts
DELETE FROM facts
WHERE id NOT IN (
  SELECT id FROM (
    SELECT id, ROW_NUMBER() OVER (
      PARTITION BY subject, predicate, object, tags
      ORDER BY confidence DESC, created_at DESC
    ) AS rn FROM facts
  ) WHERE rn = 1
);
```

---

### 16. `get_actions` 通过 IPC 序列化全量 ActionInfo（含每步 input/output）
**文件:** `crates/action/src/lib.rs:546-548`；调用 `commands.rs:174,286,301`, `crates/agent/src/lib.rs:585-591,671`

`run_action_from_id` 等只是找一个 action 就 `list_actions().clone()` 全表。增加 `get_action(id)`；`get_actions` 命令返回不含 `steps` 的精简投影。

```rust
// action/lib.rs
#[derive(Serialize)]
pub struct ActionSummary {
    pub id: String,
    pub title: String,
    pub status: ActionStatus,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
}

pub fn list_summaries(&self) -> Result<Vec<ActionSummary>> {
    // SELECT 仅必要字段，不 JOIN steps
}

pub fn find(&self, id: &str) -> Option<ActionInfo> {
    self.actions.read().ok()?.values().find(|t| t.id == id).cloned()
}
```

---

### 17. `action:*` 事件 `loadActions()` 无去重
**文件:** `ui/src/routes/+page.svelte:674-710`

后端事件突发时连续 2-4 次 `get_actions` IPC 往返。增加 microaction / `requestAnimationFrame` 合并。

```js
let loadActionsScheduled = false;
function scheduleLoadActions() {
  if (loadActionsScheduled) return;
  loadActionsScheduled = true;
  Promise.resolve().then(() => { loadActionsScheduled = false; loadActions(); });
}
// 在 action:created/updated/completed/error 处理器中调用 scheduleLoadActions()
```

---

### 18. `onScroll` 每帧写 `$state(autoFollow)`
**文件:** `ui/src/routes/+page.svelte:581-587`

每帧调度页面重渲染。已有 `scrollRafPending` 模式可复用，仅在布尔值翻转时赋值。

```js
function onScroll() {
  if (!messagesEl || scrollRafPending) return;
  scrollRafPending = true;
  requestAnimationFrame(() => {
    scrollRafPending = false;
    if (!messagesEl) return;
    const atBottom = messagesEl.scrollHeight - messagesEl.scrollTop - messagesEl.clientHeight < 100;
    if (atBottom !== autoFollow) autoFollow = atBottom;
  });
}
```

---

### 19. 长匹配字符串 / `LIKE '%…%'` 历史搜索
**文件:** `crates/memory/src/actions.rs:377-412`（`search_history_filtered`）

`LIKE '%query%'` 走全表扫描；同命令在 Tauri async runtime 同步执行。

**修复:**
- 装 FTS5 虚拟表
- 查询包 `spawn_blocking`

```sql
-- migrations.sql
CREATE VIRTUAL TABLE IF NOT EXISTS actions_fts USING fts5(
  title, content, content_rowid='id'
);
```

---

## 🟢 低优先级（确认 OK / 微优化）

### ✔ 已 OK，无需改动
- `crates/tools/src/search.rs:181-196` 已是 `ignore` 的并行 walker + mmap ripgrep + `spawn_blocking`
- `crates/llm` 的 `reqwest::Client` 池化（per-host 5, 90s idle）已正确，连接预热已做
- 所有 `listen()` 订阅都正确释放（`+page.svelte:494,897-904`, `+layout.svelte:160-169`）
- `{#each}` 全部正确 key（`+page.svelte:1142`, `history:366`）
- Release profile `opt-level=3, lto=true, codegen-units=1, strip=true` 已正确
- `crates/tools/src/file.rs`/`shell.rs` 读已做大小截断 + 异步 + 输出有上限

### 微改动
- `+layout.svelte:4-5` 遗留 `BOOT TEST 1/2` 调试 toast，每次启动都会显示 10s —— 删掉
- `ui/static/favicon.png` 是 136 字节空文件 —— 替换成真实 PNG
- `MaterialDatePicker.svelte:248-253` 每天单元计算 `todayDate()` —— 提到 `$derived`
- `+layout.svelte:23,48,97` 根 layout 订阅未 unsubscribe（实际不会泄漏）—— 标准化
- `Cargo.toml` 缺 `panic = "abort"`（可选，~5-15% 体积收益，谨慎验证 panic 语义）
- `settings/+page.svelte:111-114`, `MaterialAutocomplete.svelte:57-62` 的 fetch/blur timer 未在 `onDestroy` 清除
- `ui/vite.config.js` 可加 `manualChunks` 拆分 markdown + highlight.js
- `crates/agent/src/react.rs:1075-1079` 的 `autoGrowInput` effect 改用 `oninput` handler，避免每次按键 reflow

---

## 建议执行顺序（按 ROI 排序）

| # | 任务 | 预估收益 | 风险 |
|---|------|----------|------|
| 1 | 1. 流式 markdown 改纯文本 + 结束渲染一次 | ⭐⭐⭐⭐⭐ | 低 |
| 2 | 2. markdown-it 单例 + 非 assistant 跳过 | ⭐⭐⭐⭐ | 低 |
| 3 | 3. DB 调用走 `spawn_blocking` / async facade | ⭐⭐⭐⭐ | 中（接口面广） |
| 4 | 4. ReAct 快照改 delta + 异步写入 | ⭐⭐⭐⭐ | 中 |
| 5 | 12. `PRAGMA synchronous=NORMAL` + step 批量持久化 | ⭐⭐⭐ | 低 |
| 6 | 6, 7. `updateModelState` / `messages` 去重 store.notify | ⭐⭐⭐ | 低 |
| 7 | 8. stream-rule 增量匹配 + 9. SSE 字节级解析 | ⭐⭐⭐ | 低 |
| 8 | 13, 14. CPAL scratch buffer + VAD singleton | ⭐⭐⭐ | 中 |
| 9 | 15. `dedup_facts` window-function + 19. FTS5 | ⭐⭐ | 低 |
| 10 | 5. 长对话 `content-visibility: auto` + 窗口化 | ⭐⭐ | 中 |
| 11 | 16. `get_actions` 投影 + 17. `loadActions` 合并 | ⭐⭐ | 低 |
| 12 | 18. `onScroll` raf 节流 + 各种微改动 | ⭐ | 低 |

---

## 落地模板

每个高/中优先级任务建议工作流：

1. **基线**: 记录当前关键指标（启动时间、CPU、流式 fps、DB 写入次数）
2. **改动**: 提交到独立分支
3. **验证**: `cargo test` + `npm run check` + `npm run test:run`
4. **对比**: 同任务时间、流式卡顿次数、内存峰值
5. **记录**: 在本文件对应小节补"已实施 @ 提交哈希"链接

---

## 引用

- 完整审查报告见 `session_digest`（审查会话 ID 可通过 `kilo_memory_recall` 检索）
- 后端结构参见 `docs/Pi Coding Agent架构.md`
- UI 文档参见 `docs/ui.md`
