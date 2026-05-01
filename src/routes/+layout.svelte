<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import Sidebar from '$lib/components/layout/Sidebar.svelte';
	import Header from '$lib/components/layout/Header.svelte';
	import KeyboardHandler from '$lib/components/layout/KeyboardHandler.svelte';
	import ShortcutHelp from '$lib/components/layout/ShortcutHelp.svelte';
	import { isTauri } from '$lib/tauri/mock';
	import { autoBacktestIfDue } from '$lib/tauri/commands';
	let { children } = $props();

	onMount(() => {
		if (!isTauri()) return;
		// Fire-and-forget daily auto-backtest. Gated by day-90 threshold and
		// once-per-day guard inside the Rust command — safe to call every load.
		autoBacktestIfDue()
			.then(s => console.info('[auto-backtest]', s.message))
			.catch(e => console.warn('[auto-backtest] failed:', e));
	});
</script>

<KeyboardHandler />
<ShortcutHelp />

<div class="flex h-screen overflow-hidden bg-bg">
	<Sidebar />
	<main class="flex-1 flex flex-col overflow-hidden">
		<Header />
		<div class="flex-1 overflow-y-auto px-8 pb-8">
			{@render children()}
		</div>
	</main>
</div>
