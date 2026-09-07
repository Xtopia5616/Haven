# Haven UI 设计规范与编码规则

> 版本: v2.0 | 日期: 2026-09-03
>
> 本文同时是 Haven UI 设计系统、UX 目标和连续实施计划的唯一入口。实现过程中的组件、页面和交互决策必须回到本文校验。

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
10. [UX 重构目标与连续实施计划](#10-ux-重构目标与连续实施计划)

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

### 1.2 字体与行高

字号、字重和行高使用语义 token，禁止页面随意组合字号与 `line-height`。正文默认使用 14px / 1.55；小号正文使用 13px / 1.5；按钮和标签使用 14px / 1.4 或 12px / 1.45；标题按层级使用 18px、20px、28px 和 30px，并保持至少 1.2 的行高。

| Token | 值 | 适用场景 |
|---|---:|---|
| `--md-sys-typescale-display-size` / `-line-height` | 30px / 1.2 | 欢迎页主标题 |
| `--md-sys-typescale-headline-large-size` / `-line-height` | 28px / 1.25 | 页面主标题 |
| `--md-sys-typescale-headline-medium-size` / `-line-height` | 20px / 1.3 | 区块标题 |
| `--md-sys-typescale-title-large-size` / `-line-height` | 18px / 1.35 | 会话标题 |
| `--md-sys-typescale-body-medium-size` / `-line-height` | 14px / 1.55 | 正文、聊天内容 |
| `--md-sys-typescale-body-small-size` / `-line-height` | 13px / 1.5 | 辅助正文 |
| `--md-sys-typescale-label-medium-size` / `-line-height` | 12px / 1.45 | 状态、菜单、次要标签 |
| `--md-sys-typescale-label-small-size` / `-line-height` | 11px / 1.4 | 极少量元数据 |

### 1.3 形状系统

| Token | 值 | 适用场景 |
|---|---|---|
| `--md-sys-shape-extra-small` | 4px | 复选框、微小组件 |
| `--md-sys-shape-small` | 8px | 微小按钮、菜单项、chip |
| `--md-sys-shape-medium` | 12px | Tab、统一按钮和控制条 |
| `--md-sys-shape-large` | 16px | Section 容器 |
| `--md-sys-shape-extra-large` | 28px | Dialog |
| `--md-sys-shape-full` | 9999px | 全圆角 |

### 1.4 间距系统

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

### 1.5 动效系统

| Token | 值 | 适用场景 |
|---|---|---|
| `--md-sys-motion-easing-standard` | `cubic-bezier(0.2, 0, 0, 1)` | 通用过渡 |
| `--md-sys-motion-easing-emphasized` | `cubic-bezier(0.3, 0, 0, 1)` | 弹窗、菜单 |
| `--md-sys-motion-duration-fast` | 100ms | 颜色变化、状态层 |
| `--md-sys-motion-duration-short` | 200ms | 通用过渡 |
| `--md-sys-motion-duration-medium` | 300ms | 布局变化 |

### 1.6 阴影层级

| Token | 适用场景 |
|---|---|
| `--md-sys-elevation-0` | 默认 |
| `--md-sys-elevation-1` | 悬浮卡片、按钮 hover |
| `--md-sys-elevation-2` | 卡片 hover |
| `--md-sys-elevation-3` | 下拉菜单、Snackbar |
| `--md-sys-elevation-4` | Dialog |
| `--md-sys-elevation-5` | 最高层级 |

### 1.7 组件原始类

定义在 `app.css` 中的全局组件类：

| 类名 | 用途 | 变体 |
|---|---|---|
| `.md-btn` | 按钮 | `--filled`, `--tonal`, `--elevated`, `--outlined`, `--text`, `--danger`, `--xs` |
| `.md-icon-btn` | 图标按钮 | `data-variant`、`data-size` |
| `.md-toolbar` | 统一工具栏的对齐与间距 | — |
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
│   ├── MaterialButton.svelte
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
| 按钮 | `MaterialButton.svelte` | `.md-btn` | `--filled`, `--outlined` 等 class 修饰 |
| 卡片 | `MaterialCard.svelte` | `.md-card` | `data-variant` |
| 徽章 | `MaterialBadge.svelte` | `.md-badge` | `data-variant` |
| 图标按钮 | `MaterialIconButton.svelte` | `.md-icon-btn` | `data-variant` |
| 工具栏 | — | `.md-toolbar` | 统一对齐与间距 |
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

## 10. UX 重构目标与连续实施计划

> 本节是当前 UI/UX 重构的执行依据。实现 agent 必须先读完本节，再按 Phase 1 至 Phase 7 顺序执行；每个阶段完成、审查并通过门禁后，自动进入下一阶段，不在阶段之间等待用户确认。

### 10.1 重构目标与不可变边界

Haven 的核心场景是“快速开始一段对话，并清楚知道它当前在做什么”。界面应围绕这个场景组织，而不是让用户在页面、状态菜单、工具卡片和设置表单之间寻找当前上下文。

目标原则：

1. **对话优先**：打开应用后，用户首先看到当前会话、输入区和下一步操作；其它能力按需展开。
2. **上下文连续**：会话、工具调用、确认、后台任务、定时任务和错误都要在同一条可追溯的上下文中呈现。
3. **状态可行动**：每个“未配置、失败、等待、断开、保存中”状态都必须说明原因，并提供明确的下一步动作。
4. **渐进披露**：默认只展示完成当前任务所需的信息，详情、调试信息和低频操作放入抽屉、详情面板或二级区域。
5. **统一而有层级**：同一种功能只保留一种组件和一套视觉契约；层级通过尺寸、间距、颜色和位置表达，不通过随意混用圆角、直角和高度表达。
6. **桌面优先，窄窗可用**：以桌面窗口为主设计，同时保证 1440px、1024px、800px 和约 455px 宽度下不溢出、不遮挡主要操作。
7. **可恢复**：停止、结束、重试、撤销、放弃更改等动作的语义必须区分清楚，且在风险操作前给出必要确认。
8. **契约稳定**：优先在前端重组信息架构和交互，不随意修改 Rust 命令、数据库字段、事件名、ID 语义或持久化协议。确有必要时必须先记录影响和迁移方案。

明确边界：

- 本次重构允许重排路由、拆分页面编排、提取公共组件和重写前端状态流，但不能为了视觉效果破坏现有后端行为。
- 所有共享 token、基础控件和状态表现必须有唯一实现；禁止新旧两套按钮、Tab、状态徽章或保存栏长期并存。
- `lib/` 组件保持展示和交互抽象，业务数据加载、Tauri `invoke` 和事件订阅由路由或领域编排层负责，遵守本文第 2.4 节。

### 10.2 当前 UX 问题基线

以下问题是本轮重构必须解决的基线，不得只通过继续叠加 CSS 覆盖来掩盖：

| 问题 | 用户影响 | 重构方向 |
|---|---|---|
| 顶部导航、状态 chip、状态菜单、任务入口和页面内容分散表达 | 用户无法快速判断“我在哪”和“现在发生了什么” | 建立工作区壳层，拆分导航、会话状态和任务职责 |
| 一级 Tab、二级 Tab、底部短下划线和局部选中态存在不同高度、形状和激活规则 | 页面像由多套设计拼接而成，视觉层级不稳定 | 只保留一套 Tab 原语；不同层级只通过容器位置和强调等级区分 |
| 对话区的按钮、保存配置、新建/结束对话、未配置提示使用多种尺寸和形状 | 高频操作的肌肉记忆无法建立，窄窗下还会占用过多空间 | 统一按钮几何、尺寸、文字语义和危险级别；方形按钮用于明确的短操作，宽按钮只用于带动作语句的主要 CTA |
| `+layout.svelte` 同时承担全局壳层、页面切换、会话状态、后台任务和事件聚合 | 修改一个交互容易影响其它页面，状态来源不清晰 | 将全局壳层、会话编排、任务编排和通知聚合拆成边界明确的模块 |
| 设置页把多类设置堆在同一页面，仅用一个全局保存入口 | 用户不知道哪些内容已改、会保存什么，也不容易定位验证错误 | 按意图分组，支持分组脏状态、分组校验、保存/放弃和离开保护 |
| 工具、技能、MCP 和记忆的列表/详情/操作层级不一致 | 用户需要反复学习不同页面的操作方法 | 统一为“列表 → 详情 → 操作反馈”模式，工具能力按任务需要进入对话上下文 |
| loading、empty、error、unconfigured、saving 等状态由各页面自行解释 | 同一类情况出现不同文案、颜色和按钮 | 使用统一异步状态契约和可复用状态组件 |

审查时必须同时检查结构和视觉：如果两个相同语义的控件仍然有两套 DOM/CSS 定义，视为未完成；如果只是看起来相似但行为语义不同，必须通过组件变体或明确的 `data-variant` 表达差异。

### 10.3 目标信息架构

目标是一个“工作区壳层 + 对话主工作区 + 按需上下文”的结构：

```text
Haven 工作区
├── 工作区导航
│   ├── 对话（默认入口）
│   ├── 任务（会话 / 后台任务 / 定时任务）
│   ├── 工具
│   ├── 记忆
│   └── 设置
├── 对话工作区
│   ├── 会话栏：新建、搜索、切换、重命名、结束
│   ├── 会话头部：标题、模型、当前会话状态、会话操作
│   ├── 消息时间线：消息、思考、工具调用、确认和结果
│   └── 统一输入区：文字、语音、图片/文件、发送、停止
└── 上下文层（按需打开）
    ├── 任务详情抽屉
    ├── 工具/技能详情抽屉
    ├── 会话信息和调试详情
    └── 全局通知中心
```

信息架构规则：

- “对话”是默认首页；用户不需要先进入状态菜单才能知道当前会话是否运行。
- “任务”只负责执行态：当前会话、后台任务、定时任务和它们的执行记录。页面按“当前会话 / 后台与定时任务 / 执行记录”分组；状态菜单只保留为轻量摘要，不再承担完整任务管理。
- 对话中的工具调用、确认、后台任务和定时任务必须保留返回来源；用户从任务打开时，应能回到触发它的会话。
- 工具页采用列表/详情布局：左侧或上方是可搜索、可筛选列表，右侧或抽屉是详情与操作；不得为每个项目复制一套卡片操作逻辑。
- “记忆”只负责可回顾的信息：会话历史、长期记忆和记忆检索。会话历史可以打开并继续，但不承载任务的执行状态；任务状态统一回到“任务”查看。
- 设置页按用户目标分组，而不是按实现模块堆叠：模型与连接、语音与媒体、行为与权限、性能与限制。每组内部使用统一的表单区块和动作栏。
- 一级导航、页面内分组和局部筛选不是同一种 Tab。只有在同一上下文内切换同层内容时才使用 Tab；否则使用导航项、分段控制或筛选器。

### 10.4 交互和状态契约

所有需要异步加载或提交的领域状态，都必须能映射到以下状态集合：

```text
idle → loading → ready | empty | unconfigured | error
ready + 用户修改 → dirty → saving → saved | error
```

统一规则：

- `loading`：显示稳定的占位结构，不通过页面整体闪烁制造等待感。
- `empty`：说明为什么为空，并给出创建、搜索或导入动作；不能只显示一张空白卡片。
- `unconfigured`：解释缺少哪项配置，并提供“去配置/立即配置”动作；不能只显示灰色徽章。
- `error`：说明影响范围，提供重试；需要用户修复配置时，提供跳转到对应设置组的动作。
- `dirty`：在页面标题或分组动作栏附近显示未保存提示；离开页面、切换分组或关闭窗口时按风险提供保护。
- `saving` / `saved`：保存动作有进行中、成功和失败三种反馈；成功提示应短暂且不遮挡主内容，失败信息应保留在相关分组旁。
- `stopped` 与 `ended` 必须区分：停止是终止当前运行，允许继续同一会话；结束是关闭会话并退出当前工作上下文。
- `retry` 只针对可重试的失败；不可重试错误必须告诉用户需要改变什么。

状态展示优先级：

1. 当前会话正在录音、转写、生成、等待确认或调用工具时，状态显示在会话头部和相关消息/工具卡片附近。
2. 后台任务或定时任务显示在任务，并在会话中保留紧凑的来源和进度入口。
3. 全局顶栏只显示应用级连接、配置或同步状态；不得把多个会话、任务和连接状态压缩成一个无法解释的颜色 chip。
4. 通知 toast 只用于短暂结果（如“已保存”“已复制”）；持久错误、未配置和需要操作的状态必须留在上下文中。

交互几何契约：

- 基础按钮、输入框、Tab、徽章和卡片使用第 1 节的唯一 token；禁止在局部重新发明圆角、内边距、控件高度或下划线。
- 同一行的按钮和输入控件使用同一控件高度；主要内容区、标题栏、状态栏和底部动作栏按同一垂直节奏对齐，不能出现“顶部过窄、底部过高”的视觉失衡。
- 主按钮只保留一个明确的视觉重点；停止、结束、删除等危险动作使用危险变体，不靠位置或颜色猜测含义。
- 短图标动作使用统一方形命中区域和 `aria-label`；带完整动作语句的 CTA 才使用宽按钮。
- Tab 激活态只能由一个公共原语绘制。若采用短下划线，长度、厚度、偏移和动画必须由 token 统一控制，不能由页面自定义。

### 10.5 核心用户流程

实现时优先保证以下流程在真实数据、空数据、错误和窄窗状态下都成立：

| 流程 | 必须清楚的内容 | 完成标准 |
|---|---|---|
| 新建 / 切换会话 | 当前会话、会话列表、正在运行的会话、创建入口 | 新建后自动聚焦输入区；切换不会丢失未提交文本；运行状态可见 |
| 发送消息 | 输入方式、附件、发送和停止 | 发送后立即出现用户消息；生成中可停止；停止后可继续；失败可重试 |
| 工具调用与确认 | 工具做什么、影响范围、允许/拒绝 | 确认项出现在消息上下文；允许、拒绝和始终允许语义不同；结果可展开查看 |
| 后台任务 / 定时任务 | 来源、进度、状态、下一步 | 能从会话进入任务详情，也能从任务详情回到来源会话；取消后有明确结果 |
| 配置模型 / provider | 当前配置、验证结果、缺少字段 | 未配置状态可直达配置；验证中不可重复提交；失败定位到具体字段或连接 |
| 保存设置 | 修改范围、保存进度、失败原因 | 分组脏状态可见；保存成功不丢焦点；离开时能保存、放弃或返回编辑 |
| 结束会话 | 结束与停止的差异、潜在影响 | 结束前给出确认；结束后回到明确的会话列表或新建状态，不留下“看似可输入但实际已结束”的界面 |

### 10.6 目标组件和状态所有权

重构过程中优先建立以下可复用组件/编排边界。名称可以按现有项目命名规范调整，但职责不能合并回一个巨型页面：

| 目标边界 | 负责什么 | 不负责什么 |
|---|---|---|
| `AppShell` / 工作区壳层 | 顶栏、工作区导航、全局布局、响应式断点 | 会话业务、工具加载、设置保存 |
| `WorkspaceNav` | 一级入口、当前工作区、窄窗折叠 | 具体页面数据请求 |
| `SessionRail` | 新建、搜索、切换、会话级操作 | 生成循环、消息持久化 |
| `SessionHeader` | 标题、模型、会话状态、停止/结束等会话动作 | 任务列表和全局通知 |
| `ConversationTimeline` | 消息、思考、工具调用、确认、结果的展示编排 | 直接 `invoke` |
| `Composer` | 文字、语音、附件、发送/停止交互 | 决定后端任务状态 |
| `TaskCenter` / `TaskDrawer` | 会话、后台任务、定时任务的统一展示和操作 | 修改消息时间线内部数据 |
| `AsyncState` / `EmptyState` / `ErrorState` | 统一加载、空、未配置、错误反馈 | 领域数据获取 |
| `SettingsNav` / `SettingsSection` / `SettingsActionBar` | 设置分组、分组校验、保存/放弃动作 | 直接读取其它页面业务状态 |
| `ResourceList` / `ResourceDetail` | 工具、技能、MCP 等资源的列表/详情模式 | 为每种资源复制一套视觉系统 |

状态所有权必须沿着“领域编排 → 容器组件 → 纯展示组件”单向流动。一个状态只能有一个权威来源；组件不得通过复制 prop、重复订阅或页面级 CSS 覆盖制造第二份状态。提取组件前先用 `rg` 搜索相同结构、类名、token 和事件处理，确认是否应该复用现有实现。

### 10.7 连续实施阶段

按以下顺序推进，每个阶段都是可独立审查和回滚的垂直切片：

| 阶段 | 目标交付物 | 重点验收 |
|---|---|---|
| Phase 1：壳层与状态边界 | 工作区壳层、导航、全局状态/通知/任务的边界整理 | 页面入口清楚；状态不再由顶层巨型组件重复编排；现有命令和事件契约不变 |
| Phase 2：对话主工作区 | 会话栏、会话头部、统一时间线、统一输入区 | 新建/切换/发送/停止/结束/重试流程完整；主操作可见；窄窗不溢出 |
| Phase 3：任务 | 会话、后台任务、定时任务和已完成记录的统一列表/详情 | 任务来源、生命周期、取消和回到会话路径清晰；全局状态摘要不再替代任务 |
| Phase 4：设置工作流 | 按用户意图分组的设置导航、分组校验和动作栏 | 未配置可行动；脏状态、保存、失败、放弃和离开保护完整；按钮尺寸一致 |
| Phase 5：工具与记忆 | 工具/技能/MCP 列表详情模式；记忆分组和检索入口 | 列表、详情、操作反馈可复用；不复制卡片和状态组件；信息层级清楚 |
| Phase 6：视觉与无障碍收口 | 统一 token、控件几何、状态组件、键盘/读屏和响应式 | 全量清理重复 CSS/重复组件；1440/1024/800/455 宽度视觉审查通过 |
| Phase 7：最终清理与验收 | 删除旧入口、旧样式和兼容分支；更新文档和测试矩阵 | 无旧新双轨；全门禁通过；用户核心流程和错误/空/未配置状态全部可验证 |

每个阶段必须执行以下连续循环：

1. **阶段前检查**：阅读相关代码、现有测试和前一阶段提交；用 `rg` 建立旧结构、重复定义和事件入口清单。
2. **实现一个垂直切片**：先让一个完整用户流程贯通，再扩展到同类页面；不同时引入无关的后端重构。
3. **运行工程门禁**：至少运行 `cd ui; corepack pnpm run check`、`corepack pnpm run test:run` 和 `corepack pnpm run build`；跨端改动按项目要求补充 Rust 检查和测试。
4. **视觉与交互审查**：检查桌面和窄窗尺寸、键盘焦点、loading/empty/error/unconfigured/saving 状态、按钮命中区域和滚动行为；必要时使用可用的浏览器/桌面预览工具。
5. **静态复查**：再次搜索重复类名、重复 DOM、旧 Tab/按钮/状态入口和未使用导入；确认旧实现已被新实现替代，而不是被隐藏。
6. **提交阶段成果**：精确暂存相关文件，运行 `git diff --cached --check`，复核 staged diff，提交一个可读的 `<type>(<scope>): <imperative summary>` commit。
7. **自动推进**：只有阶段门禁、视觉审查、静态复查和提交都通过后，才进入下一阶段；不在阶段之间等待用户确认。

以下情况才允许暂停并向用户报告：需要改变后端/数据库/外部契约、需要删除或迁移用户数据、测试门禁在合理修复后仍无法通过、发现需求冲突或缺少必要权限。普通的组件拆分、样式调整、测试补充和常规重构不属于暂停理由。

### 10.8 完成定义

本轮 UX 重构只有在以下条件全部满足后才算完成：

- 用户打开应用即可进入对话并理解当前会话状态，不需要打开隐藏菜单寻找主要操作。
- 新建、切换、发送、停止、结束、重试、确认工具和查看任务均有明确且一致的路径。
- 设置、工具、记忆和任务使用相同的页面层级、列表/详情模式、状态反馈和动作语义。
- 一级导航、页面分组、筛选器和 Tab 的用途不混淆；Tab、按钮、卡片、输入框的尺寸、圆角、下划线和间距来自唯一 token/原语。
- 所有异步状态都覆盖正常、空、加载、错误、未配置、保存中和成功反馈；需要操作的状态均有下一步动作。
- 重复的组件、重复 CSS、旧入口和仅用于遮盖问题的局部覆盖已删除，而不是继续保留。
- `cd ui; corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build` 及适用的 Rust 门禁全部通过；阶段提交历史可按 Phase 追溯。

### 10.9 执行 agent 指令

执行本节的 agent 必须把本文视为任务清单和验收标准：从 Phase 1 开始，按顺序完成一项、审查一项、提交一项，再自动做下一项。每次继续前先读取当前工作树和最近提交，保留用户已有的无关修改；不要通过大爆炸式重写一次性替换所有页面，也不要为了暂时通过视觉检查而保留新旧两套实现。若遇到真正的阻塞条件，停止在当前阶段并说明证据；否则持续执行直到 Phase 7 完成并输出最终审查结果。

### 10.10 本轮实施验收矩阵

| 阶段 | 实施提交 | 验收结果 |
|---|---|---|
| Phase 1 | `e108212` | 工作区壳层、导航、状态边界；check/test/build 通过 |
| Phase 2 | `91facfb` | 会话头部、时间线、输入区；check/test/build 通过 |
| Phase 3 | `d226359` | 任务列表/详情、取消与回到会话；check/test/build 通过 |
| Phase 4 | `635af95` | 设置分组状态、未配置提示、保存/放弃与离开保护；check/test/build 通过 |
| Phase 5 | `e1b7601` | 工具资源搜索/筛选与记忆分组详情；check/test/build 通过 |
| Phase 6 | `ade5901` | 统一异步状态、响应式与可访问性收口；40 个测试文件、469 个测试通过，build 通过 |
| Phase 7 | 当前提交 | 清理旧页面实现、保留深链归一化桩并记录验收矩阵 |

旧的 `/tools`、`/memory`、`/history`、`/settings` 路由文件仅负责深链归一化；实际页面唯一实现位于工作区壳层，避免保留第二套 DOM 和交互逻辑。

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
