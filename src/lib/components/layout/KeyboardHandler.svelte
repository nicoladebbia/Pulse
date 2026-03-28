<script lang="ts">
	import { activeSector, expandedStoryId } from '$lib/stores/briefing';
	import { navigateDown, navigateUp, expandFocused } from '$lib/stores/navigation';
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

		switch (key) {
			case 'j':
				event.preventDefault();
				navigateDown();
				break;
			case 'k':
				event.preventDefault();
				navigateUp();
				break;
			case 'Enter':
				event.preventDefault();
				expandFocused();
				break;
			case 'Escape':
				expandedStoryId.set(null);
				break;
			case '/':
				event.preventDefault();
				document.querySelector<HTMLInputElement>('[data-search-input]')?.focus();
				break;
			case '?':
				event.preventDefault();
				goto('/ask');
				break;
			case 'o': {
				// Open source URL for focused story
				const focusedEl = document.querySelector('[data-focused="true"]');
				const url = focusedEl?.getAttribute('data-url');
				if (url) window.open(url, '_blank');
				break;
			}
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />
