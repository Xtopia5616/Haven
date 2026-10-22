import { writable } from 'svelte/store';

// Renderer-only layout preferences survive workspace switches, but reset when
// the application reloads. Session data remains owned by the chat route.
export const sessionSidebarLayoutStore = writable({ collapsed: false, width: 240 });
