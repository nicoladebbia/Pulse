<script lang="ts">
	import { activeSector, expandedStoryId } from '$lib/stores/briefing';
	import { goto } from '$app/navigation';
	import type { SectorId } from '$lib/config';

	const sectorKeys: Record<string, SectorId | null> = {
		'1': 'ai',
		'2': 'miami',
		'3': 'italy',
		'4': 'tech',
		'0': null,
	};

	function handleKeydown(event: KeyboardEvent) {
		// Don't handle if typing in an input
		const target = event.target as HTMLElement;
		if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable) {
			return;
		}

		const key = event.key;

		// Sector switching
		if (key in sectorKeys) {
			event.preventDefault();
			activeSector.set(sectorKeys[key]);
			return;
		}

		// Navigation shortcuts
		switch (key) {
			case 'Escape':
				expandedStoryId.set(null);
				break;
			case '/':
				event.preventDefault();
				// TODO: Focus search input
				break;
			case '?':
				event.preventDefault();
				goto('/ask');
				break;
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />
