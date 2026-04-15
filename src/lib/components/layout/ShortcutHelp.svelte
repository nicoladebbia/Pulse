<script lang="ts">
	import { showShortcutHelp } from '$lib/stores/navigation';

	function close() {
		showShortcutHelp.set(false);
	}

	function handleKeydown(event: KeyboardEvent) {
		if (!$showShortcutHelp) return;
		if (event.key === 'Escape' || event.key === '?') {
			event.preventDefault();
			close();
		}
	}

	const shortcuts = [
		{ section: 'Navigation', items: [
			{ key: 'j / k', desc: 'Move down / up through stories' },
			{ key: 'Enter', desc: 'Expand focused story' },
			{ key: 'Escape', desc: 'Close expanded story' },
			{ key: 'o', desc: 'Open source URL' },
		]},
		{ section: 'Pages', items: [
			{ key: '/', desc: 'Focus search' },
			{ key: 'f', desc: 'Go to Freedoms' },
			{ key: 'p', desc: 'Go to Ask Pulse' },
			{ key: '?', desc: 'Show this help' },
		]},
		{ section: 'Sectors (home only)', items: [
			{ key: '0', desc: 'Show all sectors' },
			{ key: '1', desc: 'Toggle AI' },
			{ key: '2', desc: 'Toggle Miami' },
			{ key: '3', desc: 'Toggle Italy' },
			{ key: '4', desc: 'Toggle Tech' },
		]},
	];
</script>

<svelte:window onkeydown={handleKeydown} />

{#if $showShortcutHelp}
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div class="fixed inset-0 z-50 flex items-center justify-center bg-black/60" onclick={close}>
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div class="bg-bg-card border border-border rounded-xl p-6 max-w-md w-full mx-4 shadow-2xl" onclick={(e) => e.stopPropagation()}>
			<div class="flex items-center justify-between mb-4">
				<h2 class="text-sm font-semibold text-text">Keyboard Shortcuts</h2>
				<button class="text-text-muted hover:text-text text-xs" onclick={close}>Esc</button>
			</div>

			<div class="space-y-4">
				{#each shortcuts as group}
					<div>
						<h3 class="text-[10px] font-medium uppercase tracking-wider text-text-muted mb-2">{group.section}</h3>
						<div class="space-y-1">
							{#each group.items as shortcut}
								<div class="flex items-center justify-between py-0.5">
									<span class="text-xs text-text-secondary">{shortcut.desc}</span>
									<kbd class="text-[10px] font-mono bg-bg border border-border rounded px-1.5 py-0.5 text-text-muted">{shortcut.key}</kbd>
								</div>
							{/each}
						</div>
					</div>
				{/each}
			</div>
		</div>
	</div>
{/if}
