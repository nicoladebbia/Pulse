<script lang="ts">
	import '../app.css';
	import Sidebar from '$lib/components/layout/Sidebar.svelte';
	import Header from '$lib/components/layout/Header.svelte';
	import KeyboardHandler from '$lib/components/layout/KeyboardHandler.svelte';
	import ShortcutHelp from '$lib/components/layout/ShortcutHelp.svelte';
	import { isTauri } from '$lib/tauri/mock';
	import { autoBacktestIfDue } from '$lib/tauri/commands';
	import { startGlobalPolling, stopPolling } from '$lib/stores/fetch';
	import { page } from '$app/stores';
	import { trackSurfaceView } from '$lib/engagement';
	let { children } = $props();

	// One view tracker for all 11 surfaces. Instrumenting each +page.svelte would
	// have been eleven edits that the twelfth route silently misses; the route id
	// is the surface name, so a new page is measured the moment it exists.
	// Guarded on the id because this $effect re-runs for any $page change (a query
	// param, a nav that lands back on the same route), and a view recorded twice is
	// indistinguishable from a visit that really happened twice.
	let lastSurface: string | null = null;
	$effect(() => {
		const routeId = $page.route.id;
		if (routeId === lastSurface) return;
		lastSurface = routeId;
		trackSurfaceView(routeId);
	});

	// NOTE: use $effect, NOT onMount. With this app's SvelteKit config (ssr=false +
	// prerender=true) the +layout onMount does not fire — the whole codebase drives
	// startup from $effect (see Sidebar/+page). onMount here silently never ran, which
	// is also why the pre-existing autoBacktestIfDue call had been dormant.
	// Start the app-lifetime poller exactly once. Deliberately NO $effect cleanup return:
	// an $effect re-run would fire the cleanup (stopPolling) and the `started` guard would
	// then skip restarting it, silently killing the poller after a few ticks. The poller
	// lives for the whole app session, so we start it once and never tear it down here.
	let started = false;
	$effect(() => {
		if (started) return;
		started = true;

		// Always-on fetch-status poller (1s). Runs regardless of whether the app started
		// the fetch, so the progress bar reflects scheduled launchd runs and crashes too.
		startGlobalPolling(1000);

		if (isTauri()) {
			// Fire-and-forget daily auto-backtest. Gated by day-90 threshold and a
			// once-per-day guard inside the Rust command — safe to call every load.
			autoBacktestIfDue()
				.then(s => console.info('[auto-backtest]', s.message))
				.catch(e => console.warn('[auto-backtest] failed:', e));
		}
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
