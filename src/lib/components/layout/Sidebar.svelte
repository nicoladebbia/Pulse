<script lang="ts">
	import { SECTORS, SECTOR_ORDER, type SectorId } from '$lib/config';
	import { activeSector } from '$lib/stores/briefing';
	import { page } from '$app/stores';

	function toggleSector(id: SectorId) {
		activeSector.update(current => current === id ? null : id);
	}

	const navItems = [
		{ href: '/', label: 'Today', icon: '◉' },
		{ href: '/archive', label: 'Archive', icon: '◫' },
		{ href: '/trends', label: 'Trends', icon: '◈' },
		{ href: '/ask', label: 'Ask Pulse', icon: '◎' },
	];
</script>

<aside class="w-56 bg-bg-sidebar border-r border-border flex flex-col shrink-0">
	<!-- Logo -->
	<div class="px-5 py-5 border-b border-border">
		<div class="flex items-center gap-2.5">
			<div class="w-3 h-3 rounded-full bg-ai animate-pulse"></div>
			<span class="text-lg font-semibold tracking-tight text-text">Pulse</span>
		</div>
	</div>

	<!-- Navigation -->
	<nav class="px-3 py-4 space-y-1">
		{#each navItems as item}
			<a
				href={item.href}
				class="flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors
					{$page.url.pathname === item.href
						? 'bg-bg-card text-text'
						: 'text-text-secondary hover:bg-bg-card hover:text-text'}"
			>
				<span class="text-base">{item.icon}</span>
				{item.label}
			</a>
		{/each}
	</nav>

	<!-- Sector filters -->
	<div class="px-3 py-4 border-t border-border">
		<p class="px-3 text-xs font-medium text-text-muted uppercase tracking-wider mb-3">Sectors</p>
		<button
			class="w-full flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors mb-1
				{$activeSector === null
					? 'bg-bg-card text-text'
					: 'text-text-secondary hover:bg-bg-card hover:text-text'}"
			onclick={() => activeSector.set(null)}
		>
			<span class="w-2 h-2 rounded-full bg-text-secondary"></span>
			All <span class="ml-auto text-xs text-text-muted">0</span>
		</button>
		{#each SECTOR_ORDER as id}
			{@const sector = SECTORS[id]}
			<button
				class="w-full flex items-center gap-3 px-3 py-2 rounded-lg text-sm transition-colors mb-1
					{$activeSector === id
						? 'bg-bg-card text-text'
						: 'text-text-secondary hover:bg-bg-card hover:text-text'}"
				onclick={() => toggleSector(id)}
			>
				<span class="w-2 h-2 rounded-full" style="background: {sector.color}"></span>
				{sector.name} <span class="ml-auto text-xs text-text-muted">{sector.key}</span>
			</button>
		{/each}
	</div>

	<!-- Keyboard hints -->
	<div class="mt-auto px-5 py-4 border-t border-border">
		<p class="text-xs text-text-muted leading-relaxed">
			<kbd class="text-text-secondary">j/k</kbd> navigate
			<kbd class="text-text-secondary">↵</kbd> expand
			<kbd class="text-text-secondary">/</kbd> search
		</p>
	</div>
</aside>
