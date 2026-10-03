<script lang="ts">
	import type { Update } from '@tauri-apps/plugin-updater';
	import { getVersion } from '@tauri-apps/api/app';
	import { isTauri } from '$lib/tauri/mock';
	import {
		CHECK_INTERVAL_MS,
		FIRST_CHECK_DELAY_MS,
		findUpdate,
		installUpdate,
		shortNotes,
		type UpdateState
	} from '$lib/updater';

	let ui = $state<UpdateState>({ kind: 'idle' });
	let current = $state<string | null>(null);
	let pending: Update | null = null;

	// A function, not an inline test, so TypeScript re-reads `ui` after an await.
	const busy = () => ui.kind === 'downloading' || ui.kind === 'restarting';

	async function checkNow() {
		// Never interrupt a download in progress with a re-check.
		if (busy()) return;
		try {
			const update = await findUpdate();
			// The person may have started installing while this check was in flight.
			if (busy()) return;
			pending = update;
			ui = update
				? { kind: 'available', version: update.version, notes: shortNotes(update.body) }
				: { kind: 'idle' };
		} catch (e) {
			// Offline or GitHub unreachable: stay quiet, the next check retries.
			console.warn('[updater] check failed:', e);
		}
	}

	async function install() {
		if (!pending) return;
		const version = pending.version;
		ui = { kind: 'downloading', version, percent: null };
		try {
			await installUpdate(pending, percent => {
				ui = { kind: 'downloading', version, percent };
			});
			ui = { kind: 'restarting' };
		} catch (e) {
			ui = { kind: 'error', message: String(e) };
		}
	}

	// $effect, not onMount: onMount doesn't fire in this app's layout (see +layout.svelte).
	let started = false;
	$effect(() => {
		if (started || !isTauri()) return;
		started = true;
		getVersion()
			.then(v => (current = v))
			.catch(() => {});
		const first = setTimeout(checkNow, FIRST_CHECK_DELAY_MS);
		const every = setInterval(checkNow, CHECK_INTERVAL_MS);
		return () => {
			clearTimeout(first);
			clearInterval(every);
			started = false;
		};
	});
</script>

{#if ui.kind === 'available'}
	<button
		class="w-full mt-2 flex flex-col items-start gap-0.5 px-3 py-2 rounded-lg text-left
			border border-ai/40 bg-ai-dim/40 hover:bg-ai-dim/70 transition-colors cursor-pointer"
		onclick={install}
		title={ui.notes || 'Install the new version and restart Pulse'}
	>
		<span class="text-sm text-text">⬆ Update to v{ui.version}</span>
		<span class="text-[10px] text-text-secondary">
			{ui.notes || 'Installs in a few seconds, then restarts'}
		</span>
	</button>
{:else if ui.kind === 'downloading'}
	<div class="w-full mt-2 px-3 py-2 rounded-lg border border-border text-[11px] text-text-secondary">
		Updating to v{ui.version}{ui.percent !== null ? ` · ${ui.percent}%` : '…'}
		<div class="mt-1.5 h-1 rounded bg-border overflow-hidden">
			<div class="h-full bg-ai transition-all" style="width: {ui.percent ?? 15}%"></div>
		</div>
	</div>
{:else if ui.kind === 'restarting'}
	<div class="w-full mt-2 text-[11px] text-center text-text-secondary">Restarting…</div>
{:else if ui.kind === 'error'}
	<button
		class="w-full mt-2 text-[10px] text-center text-miami hover:underline cursor-pointer"
		onclick={checkNow}
		title={ui.message}
	>
		Update failed — try again
	</button>
{/if}

{#if current}
	<div class="mt-2 text-[10px] text-center text-text-muted">Pulse v{current}</div>
{/if}
