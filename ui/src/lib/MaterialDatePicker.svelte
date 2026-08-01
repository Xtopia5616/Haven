<script>
	import { fade, scale } from 'svelte/transition';
	import { cubicOut } from 'svelte/easing';

	let { value = '', onChange, id = undefined, min = '', max = '' } = $props();

	let open = $state(false);
	let view = $state('calendar');
	let viewMonth = $state(0);
	let viewYear = $state(0);
	let tempValue = $state('');

	const WEEKDAYS = ['S', 'M', 'T', 'W', 'T', 'F', 'S'];
	const MONTHS = [
		'Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun',
		'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec',
	];

	let displayValue = $derived(value ? formatDisplay(value) : '');

	function formatDisplay(iso) {
		const d = new Date(iso + 'T00:00:00');
		if (isNaN(d.getTime())) return '';
		return `${d.getFullYear()}/${String(d.getMonth() + 1).padStart(2, '0')}/${String(d.getDate()).padStart(2, '0')}`;
	}

	function openPicker() {
		if (value) {
			const d = new Date(value + 'T00:00:00');
			if (!isNaN(d.getTime())) {
				viewMonth = d.getMonth();
				viewYear = d.getFullYear();
				tempValue = value;
			} else {
				initToToday();
			}
		} else {
			initToToday();
		}
		view = 'calendar';
		open = true;
	}

	function initToToday() {
		const now = new Date();
		viewMonth = now.getMonth();
		viewYear = now.getFullYear();
		tempValue = '';
	}

	function daysInMonth(y, m) {
		return new Date(y, m + 1, 0).getDate();
	}

	function firstDayOfMonth(y, m) {
		return new Date(y, m, 1).getDay();
	}

	function todayDate() {
		const n = new Date();
		return { y: n.getFullYear(), m: n.getMonth(), d: n.getDate() };
	}

	function tempDateObj() {
		if (!tempValue) return null;
		const d = new Date(tempValue + 'T00:00:00');
		if (isNaN(d.getTime())) return null;
		return { y: d.getFullYear(), m: d.getMonth(), d: d.getDate() };
	}

	let weeks = $derived.by(() => {
		const dim = daysInMonth(viewYear, viewMonth);
		const fd = firstDayOfMonth(viewYear, viewMonth);
		const w = [];
		let row = [];
		for (let i = 0; i < fd; i++) row.push(null);
		for (let d = 1; d <= dim; d++) {
			row.push(d);
			if (row.length === 7) { w.push(row); row = []; }
		}
		if (row.length > 0) {
			while (row.length < 7) row.push(null);
			w.push(row);
		}
		return w;
	});

	function prevMonth() {
		if (viewMonth === 0) { viewMonth = 11; viewYear--; }
		else viewMonth--;
	}

	function nextMonth() {
		if (viewMonth === 11) { viewMonth = 0; viewYear++; }
		else viewMonth++;
	}

	function selectDay(day) {
		const m = String(viewMonth + 1).padStart(2, '0');
		const d = String(day).padStart(2, '0');
		tempValue = `${viewYear}-${m}-${d}`;
	}

	function confirm() {
		onChange?.(tempValue);
		open = false;
	}

	function cancel() {
		open = false;
	}

	function goToYearView() {
		view = 'year';
	}

	function selectYear(year) {
		viewYear = year;
		view = 'calendar';
	}

	let yearGrid = $derived.by(() => {
		const start = Math.floor(viewYear / 12) * 12;
		const years = [];
		for (let i = 0; i < 12; i++) years.push(start + i);
		return { years, start };
	});

	function prevYearPage() {
		viewYear -= 12;
	}

	function nextYearPage() {
		viewYear += 12;
	}

	function headerLabel() {
		if (tempValue) {
			const d = new Date(tempValue + 'T00:00:00');
			const dayNames = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
			return `${dayNames[d.getDay()]}, ${MONTHS[d.getMonth()]} ${d.getDate()}`;
		}
		return 'Select date';
	}

	function headerYear() {
		if (tempValue) {
			return new Date(tempValue + 'T00:00:00').getFullYear();
		}
		return viewYear;
	}

	function handleKeydown(e) {
		if (e.key === 'Escape') {
			if (view === 'year') { view = 'calendar'; }
			else cancel();
		}
	}

	function handleOverlayClick(e) {
		if (e.target === e.currentTarget) cancel();
	}

	function handleNonInteractiveKeydown(e) {
		if (e.key === 'Enter' || e.key === ' ') e.preventDefault();
	}

	function isDateDisabled(year, month, day) {
		const date = new Date(year, month, day);
		if (min) {
			const minDate = new Date(min + 'T00:00:00');
			if (date < minDate) return true;
		}
		if (max) {
			const maxDate = new Date(max + 'T00:00:00');
			if (date > maxDate) return true;
		}
		return false;
	}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="md-datepicker-container">
	<button
		{id}
		class="md-datepicker-trigger"
		onclick={openPicker}
		type="button"
	>
		<span class="md-datepicker-value" class:placeholder={!value}>
			{value ? displayValue : 'yyyy/mm/dd'}
		</span>
		<svg
			class="md-datepicker-icon"
			width="20" height="20" viewBox="0 0 24 24" fill="none"
			stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"
		>
			<rect x="3" y="4" width="18" height="18" rx="2" ry="2" />
			<line x1="16" y1="2" x2="16" y2="6" />
			<line x1="8" y1="2" x2="8" y2="6" />
			<line x1="3" y1="10" x2="21" y2="10" />
		</svg>
	</button>
</div>

{#if open}
	<div
		class="md-datepicker-overlay"
		onclick={handleOverlayClick}
		role="dialog"
		aria-modal="true"
		tabindex="-1"
		onkeydown={handleNonInteractiveKeydown}
		in:fade={{ duration: 200, easing: cubicOut }}
	>
		<div class="md-datepicker-dialog" in:scale={{ start: 0.92, duration: 300, easing: cubicOut }}>
			<div class="md-datepicker-header">
				<span class="md-datepicker-header-label">{headerLabel()}</span>
				<button class="md-datepicker-header-year" onclick={goToYearView} type="button" aria-label="Switch to year view">
					{headerYear()}
				</button>
			</div>

			{#if view === 'calendar'}
				<div class="md-datepicker-nav">
					<button class="md-datepicker-nav-btn" onclick={prevMonth} type="button" aria-label="Previous month">
						<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M15 18l-6-6 6-6" />
						</svg>
					</button>
					<button class="md-datepicker-nav-label" onclick={goToYearView} type="button" aria-label="Switch to year view">
						{MONTHS[viewMonth]} {viewYear}
					</button>
					<button class="md-datepicker-nav-btn" onclick={nextMonth} type="button" aria-label="Next month">
						<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M9 18l6-6-6-6" />
						</svg>
					</button>
				</div>
				<div class="md-datepicker-weekdays">
					{#each WEEKDAYS as d}
						<span class="md-datepicker-weekday">{d}</span>
					{/each}
				</div>
				<div class="md-datepicker-grid">
					{#each weeks as week}
						{#each week as day}
							{@const t = todayDate()}
							{@const sel = tempDateObj()}
							{@const isToday = day !== null && viewYear === t.y && viewMonth === t.m && day === t.d}
							{@const isSel = day !== null && sel !== null && viewYear === sel.y && viewMonth === sel.m && day === sel.d}
							{@const isDisabled = day !== null && isDateDisabled(viewYear, viewMonth, day)}
							{#if day !== null}
								<button
									class="md-datepicker-day"
									class:today={isToday}
									class:selected={isSel}
									class:disabled={isDisabled}
									onclick={() => selectDay(day)}
									ondblclick={confirm}
									type="button"
									disabled={isDisabled}
								>
									{day}
								</button>
							{:else}
								<span class="md-datepicker-day empty"></span>
							{/if}
						{/each}
					{/each}
				</div>
			{:else}
				<div class="md-datepicker-nav">
					<button class="md-datepicker-nav-btn" onclick={prevYearPage} type="button" aria-label="Previous year range">
						<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M15 18l-6-6 6-6" />
						</svg>
					</button>
					<span class="md-datepicker-nav-label">
						{yearGrid.start} – {yearGrid.start + 11}
					</span>
					<button class="md-datepicker-nav-btn" onclick={nextYearPage} type="button" aria-label="Next year range">
						<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
							<path d="M9 18l6-6-6-6" />
						</svg>
					</button>
				</div>
				<div class="md-datepicker-year-grid">
					{#each yearGrid.years as y}
						{@const isCurrent = (new Date()).getFullYear() === y}
						{@const isSel = tempValue && new Date(tempValue + 'T00:00:00').getFullYear() === y}
						{@const isView = viewYear === y}
						<button
							class="md-datepicker-year-btn"
							class:today={isCurrent}
							class:selected={isView || isSel}
							onclick={() => selectYear(y)}
							type="button"
						>
							{y}
						</button>
					{/each}
				</div>
			{/if}

			<div class="md-datepicker-footer">
				<button class="md-btn md-btn--text" onclick={cancel}>Cancel</button>
				<button class="md-btn md-btn--filled" onclick={confirm} disabled={!tempValue}>OK</button>
			</div>
		</div>
	</div>
{/if}

<style>
	.md-datepicker-container {
		position: relative;
		width: 100%;
	}

	/* ---------- Trigger ---------- */
	.md-datepicker-trigger {
		display: flex;
		align-items: center;
		justify-content: space-between;
		width: 100%;
		height: var(--md-comp-textfield-container-height);
		padding: 0 var(--md-sys-space-lg);
		font-family: inherit;
		font-size: 15px;
		color: var(--md-sys-color-on-surface);
		background: transparent;
		border: 1px solid var(--md-sys-color-outline);
		border-radius: var(--md-comp-textfield-corner);
		cursor: pointer;
		text-align: left;
		transition: border-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			border-width var(--md-sys-motion-duration-fast) var(--md-sys-motion-easing-emphasized);
	}
	.md-datepicker-trigger:hover {
		border-color: var(--md-sys-color-on-surface);
	}
	.md-datepicker-trigger:focus-visible {
		border-color: var(--md-sys-color-primary);
		border-width: 2px;
		padding: 0 calc(var(--md-sys-space-lg) - 1px);
	}
	.md-datepicker-value {
		flex: 1;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.md-datepicker-value.placeholder {
		color: var(--md-sys-color-on-surface-variant);
	}
	.md-datepicker-icon {
		flex-shrink: 0;
		color: var(--md-sys-color-on-surface-variant);
		margin-left: var(--md-sys-space-sm);
	}

	/* ---------- Overlay & Dialog ---------- */
	.md-datepicker-overlay {
		position: fixed;
		inset: 0;
		background: color-mix(in srgb, var(--md-sys-color-scrim) 60%, transparent);
		backdrop-filter: blur(6px);
		display: flex;
		align-items: center;
		justify-content: center;
		z-index: var(--md-sys-z-dialog);
	}
	.md-datepicker-dialog {
		background: var(--md-sys-color-surface-container-high);
		border-radius: var(--md-sys-shape-large);
		box-shadow: var(--md-sys-elevation-4);
		width: 360px;
		max-width: 90vw;
		overflow: hidden;
	}

	/* ---------- Header ---------- */
	.md-datepicker-header {
		display: flex;
		flex-direction: column;
		gap: var(--md-sys-space-xs);
		padding: var(--md-sys-space-lg) var(--md-sys-space-xl);
		background: var(--md-sys-color-surface-container-low);
	}
	.md-datepicker-header-label {
		font-size: 14px;
		color: var(--md-sys-color-on-surface-variant);
		font-weight: 500;
	}
	.md-datepicker-header-year {
		font-size: 36px;
		font-weight: 400;
		color: var(--md-sys-color-on-surface);
		font-family: inherit;
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
		text-align: left;
		line-height: 1.1;
		transition: color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-datepicker-header-year:hover {
		color: var(--md-sys-color-primary);
	}

	/* ---------- Navigation ---------- */
	.md-datepicker-nav {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
	}
	.md-datepicker-nav-label {
		font-size: 14px;
		font-weight: 500;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		background: none;
		border: none;
		font-family: inherit;
		padding: 4px 8px;
		border-radius: var(--md-sys-shape-small);
		transition: background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-datepicker-nav-label:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-datepicker-nav-btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 36px;
		height: 36px;
		border: none;
		background: none;
		border-radius: var(--md-sys-shape-full);
		color: var(--md-sys-color-on-surface-variant);
		cursor: pointer;
		transition: background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-datepicker-nav-btn:hover {
		background: var(--md-sys-color-surface-container-highest);
		color: var(--md-sys-color-on-surface);
	}

	/* ---------- Weekday headers ---------- */
	.md-datepicker-weekdays {
		display: grid;
		grid-template-columns: repeat(7, 1fr);
		padding: 0 var(--md-sys-space-md);
	}
	.md-datepicker-weekday {
		text-align: center;
		font-size: 11px;
		font-weight: 500;
		color: var(--md-sys-color-on-surface-variant);
		height: 32px;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	/* ---------- Day grid ---------- */
	.md-datepicker-grid {
		display: grid;
		grid-template-columns: repeat(7, 1fr);
		padding: 0 var(--md-sys-space-md) var(--md-sys-space-sm);
	}
	.md-datepicker-day {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		aspect-ratio: 1;
		border: none;
		background: none;
		border-radius: var(--md-sys-shape-full);
		font-family: inherit;
		font-size: 13px;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		transition: background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
		width: 40px;
		height: 40px;
		margin: 0 auto;
	}
	.md-datepicker-day.empty {
		cursor: default;
	}
	.md-datepicker-day:hover:not(.empty) {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-datepicker-day.today {
		color: var(--md-sys-color-primary);
		font-weight: 700;
	}
	.md-datepicker-day.selected {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font-weight: 500;
	}
	.md-datepicker-day.selected.today {
		color: var(--md-sys-color-on-primary);
	}
	.md-datepicker-day.selected:hover {
		background: var(--md-sys-color-primary);
	}
	.md-datepicker-day.disabled {
		color: color-mix(in srgb, var(--md-sys-color-on-surface) 38%, transparent);
		cursor: default;
		pointer-events: none;
	}

	/* ---------- Year grid ---------- */
	.md-datepicker-year-grid {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md) var(--md-sys-space-md);
	}
	.md-datepicker-year-btn {
		display: flex;
		align-items: center;
		justify-content: center;
		height: 44px;
		border: none;
		background: none;
		border-radius: var(--md-sys-shape-medium);
		font-family: inherit;
		font-size: 14px;
		color: var(--md-sys-color-on-surface);
		cursor: pointer;
		transition: background-color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard),
			color var(--md-sys-motion-duration-short) var(--md-sys-motion-easing-standard);
	}
	.md-datepicker-year-btn:hover {
		background: var(--md-sys-color-surface-container-highest);
	}
	.md-datepicker-year-btn.today {
		color: var(--md-sys-color-primary);
		font-weight: 700;
	}
	.md-datepicker-year-btn.selected {
		background: var(--md-sys-color-primary);
		color: var(--md-sys-color-on-primary);
		font-weight: 500;
	}
	.md-datepicker-year-btn.selected.today {
		color: var(--md-sys-color-on-primary);
	}

	/* ---------- Footer ---------- */
	.md-datepicker-footer {
		display: flex;
		justify-content: flex-end;
		gap: var(--md-sys-space-sm);
		padding: var(--md-sys-space-sm) var(--md-sys-space-md);
		border-top: 1px solid var(--md-sys-color-outline-variant);
	}
</style>
