<script lang="ts">
	import { activeSectors, expandedStoryId } from '$lib/stores/briefing';
	import { navigateDown, navigateUp, expandFocused, showShortcutHelp } from '$lib/stores/navigation';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import type { SectorId } from '$lib/config';

	const sectorKeys: Record<string, SectorId> = {
		'1': 'ai',
		'2': 'miami',
		'3': 'italy',
		'4': 'tech',
	};

	function handleKeydown(event: KeyboardEvent) {
		const target = event.target as HTMLElement;
		if (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.isContentEditable) {
			return;
		}

		const key = event.key;

		// 0 = show all (home page only)
		if (key === '0' && $page.url.pathname === '/') {
			event.preventDefault();
			activeSectors.set([]);
			return;
		}

		// 1-4 = toggle sector (home page only)
		if (key in sectorKeys && $page.url.pathname === '/') {
			event.preventDefault();
			const sector = sectorKeys[key];
			activeSectors.update(current => {
				if (current.includes(sector)) {
					return current.filter(s => s !== sector);
				} else {
					return [...current, sector];
				}
			});
			return;
		}

		const isHome = $page.url.pathname === '/';

		switch (key) {
			case 'j':
				if (!isHome) return;
				event.preventDefault();
				navigateDown();
				requestAnimationFrame(() => {
					document.querySelector('[data-focused="true"]')?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
				});
				break;
			case 'k':
				if (!isHome) return;
				event.preventDefault();
				navigateUp();
				requestAnimationFrame(() => {
					document.querySelector('[data-focused="true"]')?.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
				});
				break;
			case 'Enter':
				if (!isHome) return;
				event.preventDefault();
				expandFocused();
				break;
			case 'Escape':
				if (!isHome) return;
				expandedStoryId.set(null);
				break;
			case '/':
				event.preventDefault();
				document.querySelector<HTMLInputElement>('[data-search-input]')?.focus();
				break;
			case '?':
				event.preventDefault();
				showShortcutHelp.update(v => !v);
				break;
			case 'f':
				event.preventDefault();
				goto('/freedoms');
				break;
			case 'p':
				event.preventDefault();
				goto('/ask');
				break;
			case 'o': {
				if (!isHome) return;
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
