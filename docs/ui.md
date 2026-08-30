# Haven UI 设计规范与编码规则

> 版本: v1.0 | 日期: 2026-07-21

---

## 目录

1. [设计系统](#1-设计系统)
2. [组件约定](#2-组件约定)
3. [CSS 命名与样式规则](#3-css-命名与样式规则)
4. [状态管理](#4-状态管理)
5. [事件处理](#5-事件处理)
6. [Tauri 桥接](#6-tauri-桥接)
7. [可访问性](#7-可访问性)
8. [文件与目录结构](#8-文件与目录结构)
9. [Svelte 5 语法规则](#9-svelte-5-语法规则)

---

## 1. 设计系统

基于 **Material Design 3 Expressive** token 系统，所有 token 定义在 `ui/src/app.css`。

### 1.1 颜色系统

使用 CSS 自定义属性，通过 `[data-theme="light|dark"]` 切换。

| Token 类别 | 命名模式 | 示例 |
|---|---|---|
| 主色 | `--md-sys-color-primary` | `#3378D6` |
| 主色上文字 | `--md-sys-color-on-primary` | `#ffffff` |
| 主色容器 | `--md-sys-color-primary-container` | `#d7e3ff` |
| 主色容器上文字 | `--md-sys-color-on-primary-container` | `#001b3e` |
| 次要色 | `--md-sys-color-secondary` | ... |
| 第三色 | `--md-sys-color-tertiary` | ... |
| 错误色 | `--md-sys-color-error` | `#ba1a1a` |
| 成功色 | `--md-sys-color-success` | `#2e7d32` |
| 警告色 | `--md-sys-color-warning` | `#7d5700` |
| 背景/表面 | `--md-sys-color-surface` | ... |
| 表面变体 | `--md-sys-color-surface-variant` | ... |
| 轮廓线 | `--md-sys-color-outline` | `#777680` |
| 轮廓线变体 | `--md-sys-color-outline-variant` | `#c7c5d0` |

**规则**:

- 不允许在组件样式中硬编码颜色值，必须使用 CSS 变量
- 不允许使用 `color-mix()` 之外的颜色函数直接操作颜色值
- 组件变体使用 `data-variant` 属性驱动主题切换

### 1.2 形状系统

| Token | 值 | 适用场景 |
|---|---|---|
| `--md-sys-shape-extra-small` | 4px | 复选框、微小组件 |
| `--md-sys-shape-small` | 8px | 按钮、输入框、卡片、chip |
| `--md-sys-shape-medium` | 12px | 历史卡片 |
| `--md-sys-shape-large` | 16px | Section 容器 |
| `--md-sys-shape-extra-large` | 28px | Dialog |
| `--md-sys-shape-full` | 9999px | 全圆角 |

### 1.3 间距系统

| Token | 值 |
|---|---|
| `--md-sys-space-xs` | 4px |
| `--md-sys-space-sm` | 8px |
| `--md-sys-space-md` | 12px |
| `--md-sys-space-lg` | 16px |
| `--md-sys-space-xl` | 20px |
| `--md-sys-space-2xl` | 24px |
| `--md-sys-space-3xl` | 32px |
| `--md-sys-space-4xl` | 48px |

### 1.4 动效系统

| Token | 值 | 适用场景 |
|---|---|---|
| `--md-sys-motion-easing-standard` | `cubic-bezier(0.2, 0, 0, 1)` | 通用过渡 |
| `--md-sys-motion-easing-emphasized` | `cubic-bezier(0.3, 0, 0, 1)` | 弹窗、菜单 |
| `--md-sys-motion-duration-fast` | 100ms | 颜色变化、状态层 |
| `--md-sys-motion-duration-short` | 200ms | 通用过渡 |
| `--md-sys-motion-duration-medium` | 300ms | 布局变化 |

### 1.5 阴影层级

| Token | 适用场景 |
|---|---|
| `--md-sys-elevation-0` | 默认 |
| `--md-sys-elevation-1` | 悬浮卡片、按钮 hover |
| `--md-sys-elevation-2` | 卡片 hover |
| `--md-sys-elevation-3` | 下拉菜单、Snackbar |
| `--md-sys-elevation-4` | Dialog |
| `--md-sys-elevation-5` | 最高层级 |

### 1.6 组件原始类

定义在 `app.css` 中的全局组件类：

| 类名 | 用途 | 变体 |
|---|---|---|
| `.md-btn` | 按钮 | `--filled`, `--tonal`, `--elevated`, `--outlined`, `--text`, `--danger`, `--xs` |
| `.md-icon-button` | 图标按钮 | `--filled` |
| `.md-input` | 文本输入框 | - |
| `.md-textarea` | 多行输入 | - |
| `.md-card` | 卡片容器 | `--elevated`, `--outlined` |
| `.md-chip` | 标签 chip | - |
| `.md-divider` | 分割线 | - |
| `.md-tabs` / `.md-tab` | Tab 导航 | `active` |
| `.md-badge` | 状态徽章 | `data-variant` 属性 |
| `.md-slider` | 滑块 | - |

---

## 2. 组件约定

### 2.1 Props 定义

所有组件使用 Svelte 5 `$props()` 解构语法，禁止 `export let`：

```svelte
<script>
 let { value = '', min = undefined, max = undefined, onChange, id = undefined } = $props();
</script>
```

**规则**:

- 为每个 prop 提供默认值
- 回调 prop 使用 `on` 前缀命名（`onChange`, `onClick`, `onClose`）
- 回调调用使用可选链 `onChange?.(val)`
- 使用 JSDoc 注释说明 prop 的用途和类型

### 2.2 Children / Snippets

容器组件使用隐式 `children` snippet：

```svelte
<script>
 let { children } = $props();
</script>
{@render children?.()}
```

消费方使用 `{#snippet}` 传递命名 snippet：

```svelte
<MaterialDialog {open} onClose={...} title="...">
 {#snippet children()}
  <p>Content</p>
 {/snippet}
 {#snippet footer()}
  <button>Cancel</button>
  <button>Confirm</button>
 {/snippet}
</MaterialDialog>
```

### 2.3 组件文件模板

```svelte
<script>
 /**
  * ComponentName — 简短描述
  * @prop {type} propName — 描述
  */
 let { prop1 = default, prop2, children } = $props();

 let localState = $state(false);
</script>

<!-- 模板 -->

<style>
 /* 组件样式 */
</style>
```

### 2.4 组件职责边界

| 组件类型 | 职责 | 禁止 |
|---|---|---|
| **`lib/` 组件** | 纯展示、可复用、无业务逻辑 | 直接调用 `invoke`、访问 store |
| **`routes/` 页面** | 业务编排、数据加载、invoke 调用 | 直接操作 DOM |
| **`lib/` 容器组件** | 布局、状态提升 | 业务逻辑 |

例外：`MaterialDialog` 可以接收 `onClose` 回调；`MaterialSwitch` 接收 `onChange` 回调。

---

## 3. CSS 命名与样式规则

### 3.1 命名约定

| 层级 | 命名风格 | 示例 |
|---|---|---|
| 全局组件类 | `md-` 前缀 | `.md-btn`, `.md-card`, `.md-input` |
| 组件内部类 | 连字符分隔 | `.history-item`, `.select-checkbox`, `.card-header` |
| 变体类 | `data-variant` 属性 | `[data-variant='primary']` |
| 状态类 | Svelte `class:` 指令 | `class:selected`, `class:expanded`, `class:open` |
| 子元素类 | 连字符，父级前缀 | `.card-header`, `.card-actions`, `.form-row` |

### 3.2 样式规则

1. **所有颜色值必须使用 CSS 变量**，禁止硬编码
2. **间距使用 `--md-sys-space-*` 变量**，禁止硬编码 px
3. **圆角使用 `--md-sys-shape-*` 变量**
4. **过渡动画使用 `--md-sys-motion-*` 变量**
5. **阴影使用 `--md-sys-elevation-*` 变量**
6. **组件内样式使用 `<style>` 块**，不写全局样式
7. **全局样式只写在 `app.css`** 中
8. **变体样式使用 `data-variant` 属性选择器**，避免多条件 class 判断

### 3.3 状态层模式

所有可交互元素实现 M3 state layer：

```css
.interactive-element {
 position: relative;
 overflow: hidden;
}
.interactive-element::after {
 content: '';
 position: absolute;
 inset: 0;
 background: currentColor;
 opacity: 0;
 transition: opacity var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
 pointer-events: none;
}
.interactive-element:hover::after {
 opacity: var(--md-sys-state-hover-opacity);
}
.interactive-element:focus-visible::after {
 opacity: var(--md-sys-state-focus-opacity);
}
.interactive-element:active::after {
 opacity: var(--md-sys-state-pressed-opacity);
}
```

### 3.4 动画模式

菜单/弹窗入场使用 `keyframes`：

```css
@keyframes menuIn {
 from { opacity: 0; transform: translateY(-4px); }
 to { opacity: 1; transform: translateY(0); }
}
```

---

## 4. 状态管理

### 4.1 分层架构

```
component-local ($state / $derived)
    ↑  props / callbacks  ↓
route-level ($state, invoke 调用)
    ↑  subscribe  ↓
shared stores (writable stores in stores.js / themeStore.js)
```

### 4.2 组件本地状态

使用 Svelte 5 runes：

```svelte
<script>
 let open = $state(false);
 let count = $state(0);
 let items = $state([]);

 let doubled = $derived(count * 2);
 let display = $derived.by(() => {
  return items.map(i => i.name).join(', ');
 });
</script>
```

### 4.3 跨组件共享状态

使用 `svelte/store` `writable`，定义在 `stores.js` 中：

```js
export const messagesStore = writable([]);
export const notificationStore = writable([]);
```

在组件中订阅：

```js
import { notificationStore } from '$lib/stores.js';

let items = $state([]);
notificationStore.subscribe((v) => (items = v));
```

### 4.4 $effect 使用场景

- 同步 prop 到本地状态（类似 watch）
- 响应式执行副作用（如自动滚动）
- 监听 URL 变化

**禁止**在 `$effect` 中修改 `$state` 变量（会导致无限循环），除非有明确的条件守卫。

---

## 5. 事件处理

### 5.1 DOM 事件

使用 Svelte 5 事件语法（小写 `on` 前缀）：

```svelte
<button onclick={handler}>Click</button>
<input oninput={handleInput} />
<div onkeydown={handleKeydown}>
```

### 5.2 事件冒泡控制

在可点击卡片或容器中，子操作按钮必须阻止冒泡：

```svelte
<button onclick={(e) => { e.stopPropagation(); onEdit?.(); }}>
 Edit
</button>
```

### 5.3 键盘事件

全局键盘监听使用 `<svelte:window>`：

```svelte
<svelte:window onkeydown={handleKeydown} />
```

### 5.4 回调约定

- 回调 prop 命名：`onChange`, `onClick`, `onClose`, `onToggle`, `onSelect`, `onConfirm`
- 调用时使用可选链：`onConfirm?.(result)`
- 回调参数尽量简洁：`onChange(v)` 而非 `onChange({ value: v })`

---

## 6. Tauri 桥接

### 6.1 调用规则

所有 Tauri 调用通过 `$lib/tauri.js` 的 `invoke` 函数，禁止直接使用 `@tauri-apps/api`：

```js
import { invoke } from '$lib/tauri.js';

async function loadData() {
 try {
  const result = await invoke('command_name', { arg1: val1 });
  // 处理结果
 } catch {
  // 静默失败（非 Tauri 环境）
 }
}
```

### 6.2 事件监听

在 `onMount` 中注册，`onDestroy` 中清理：

```js
let unlisteners = [];

onMount(() => {
 const unlisten = await listen('event:name', (event) => {
  // 处理事件
 });
 unlisteners.push(unlisten);
});

onDestroy(() => {
 unlisteners.forEach(fn => fn());
});
```

### 6.3 错误处理

- 所有 `invoke` 调用必须包裹在 `try/catch` 中
- 非 Tauri 环境（浏览器开发）静默失败
- 用户可见错误使用 `addNotification` 或 UI 提示

---

## 7. 可访问性

| 模式 | 要求 |
|---|---|
| 可交互元素 | `role="button"` + `tabindex="0"` + `onkeydown` 处理 Enter/Space |
| 图标按钮 | `aria-label` 描述操作 |
| 复选框 | 隐式 `<label>` 包裹或 `aria-label`/`aria-labelledby` |
| 弹窗 | `role="dialog"` + `aria-modal="true"` |
| 状态区域 | `aria-live="assertive"` + `role="status"` |
| 列表 | `role="listbox"` + `role="option"` + `aria-selected` |
| 展开控件 | `aria-expanded` + `aria-haspopup` |

---

## 8. 文件与目录结构

```
ui/src/
├── app.css                 # 全局 token + 组件原始类
├── app.html                # SvelteKit shell
├── lib/
│   ├── components/         # 预留复合组件目录（当前为空）
│   ├── stores.js           # 共享 writable stores
│   ├── tauri.js            # Tauri 桥接懒加载
│   ├── themeStore.js       # 主题管理
│   ├── ChatBubble.svelte   # 聊天气泡
│   ├── ConfirmationDialog.svelte
│   ├── Logo.svelte
│   ├── MaterialBadge.svelte
│   ├── MaterialCard.svelte
│   ├── MaterialDialog.svelte
│   ├── MaterialIconButton.svelte
│   ├── MaterialNumberField.svelte
│   ├── MaterialSection.svelte
│   ├── MaterialSelect.svelte
│   ├── MaterialSwitch.svelte
│   ├── McpEditDialog.svelte
│   ├── McpServerCard.svelte
│   ├── NotificationToast.svelte
│   ├── RecordingIndicator.svelte
│   ├── SkillCard.svelte
│   ├── SkillDetailDrawer.svelte
│   └── ActionCard.svelte
└── routes/
    ├── +layout.svelte      # 布局 + 事件总线
    ├── +page.svelte        # 聊天页
    ├── history/
    │   └── +page.svelte    # 历史页
    ├── settings/
    │   └── +page.svelte    # 设置页
    └── tools/
        └── +page.svelte    # 工具页
```

### 8.1 Material 组件命名规则

`lib/` 中的 Material 组件遵循以下命名：

| 组件 | 文件名 | 类名 | 变体属性 |
|---|---|---|---|
| 按钮 | — | `.md-btn` | `--filled`, `--outlined` 等 class 修饰 |
| 卡片 | `MaterialCard.svelte` | `.md-card` | `data-variant` |
| 徽章 | `MaterialBadge.svelte` | `.md-badge` | `data-variant` |
| 图标按钮 | `MaterialIconButton.svelte` | `.md-icon-btn` | `data-variant` |
| 对话框 | `MaterialDialog.svelte` | `.md-dialog` | — |
| 切换开关 | `MaterialSwitch.svelte` | `.md-switch-track` | `:checked` 伪类 |
| 数字输入 | `MaterialNumberField.svelte` | `.md-number-field` | — |
| 下拉菜单 | `MaterialSelect.svelte` | `.md-select-container` | — |

---

## 9. Svelte 5 语法规则

### 9.1 强制规则

| 语法 | 允许 | 禁止 |
|---|---|---|
| Props | `$props()` 解构 | `export let` |
| 响应式状态 | `$state()`, `$derived()`, `$derived.by()` | Svelte 4 `$:` 标签 |
| 副作用 | `$effect()` | Svelte 4 `$:` 响应式赋值 |
| 内容分发 | `{@render children?.()}`, `{#snippet}` | Svelte 4 `<slot>` |
| 双向绑定 | `bind:value` 用于表单元素 | 组件间 `bind:prop` |

### 9.2 推荐模式

- `$derived` 用于简单派生值
- `$derived.by()` 用于需要多条语句的派生
- `$state` 初始化空数组：`$state([])`，空对象：`$state({})`
- Set 类型响应式：通过创建新实例触发更新 `selectedIds = new Set(next)`

### 9.3 与 Svelte 4 store 的桥接

当需要从 `svelte/store` 的 `writable` 读取数据时：

```js
import { notificationStore } from '$lib/stores.js';

let items = $state([]);
notificationStore.subscribe((v) => (items = v));
```

不要在 Svelte 5 组件中创建新的 `writable` store，使用 `$state` 替代。

---

## 附录 A: 常见反模式

| 反模式 | 正确做法 |
|---|---|
| 硬编码颜色 `color: #3378D6` | 使用 `var(--md-sys-color-primary)` |
| 硬编码间距 `padding: 16px` | 使用 `var(--md-sys-space-lg)` |
| 使用 `<slot>` | 使用 `{@render children?.()}` |
| 使用 `export let` | 使用 `$props()` 解构 |
| 使用 `$:` 响应式标签 | 使用 `$derived` 或 `$effect` |
| 组件内直接调用 `invoke` | 在路由页面调用，通过 props 传入 |
| 在 `$effect` 中修改 `$state` 变量 | 使用事件处理器或 `$derived` |
| 重复的 CSS 代码（如多个组件实现切换开关） | 提取为公共组件 |

## 附录 B: 快速参考

### 创建新组件

```svelte
<script>
 /** @prop {string} title — 标题 */
 let { title = '', children } = $props();
</script>

<div class="component">
 {@render children?.()}
</div>

<style>
 .component {
  border-radius: var(--md-sys-shape-medium);
  padding: var(--md-sys-space-lg);
  color: var(--md-sys-color-on-surface);
 }
</style>
```

### 创建新页面

```svelte
<script>
 import { onMount } from 'svelte';
 import { invoke } from '$lib/tauri.js';

 let data = $state([]);

 onMount(async () => {
  try {
   data = await invoke('command_name');
  } catch {}
 });
</script>
```
