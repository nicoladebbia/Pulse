<script lang="ts">
	import '../app.css';
	import Sidebar from '$lib/components/layout/Sidebar.svelte';
	import Header from '$lib/components/layout/Header.svelte';
	import KeyboardHandler from '$lib/components/layout/KeyboardHandler.svelte';
	let { children } = $props();
</script>

<KeyboardHandler />

<div class="flex h-screen overflow-hidden bg-bg">
	<Sidebar />
	<main class="flex-1 flex flex-col overflow-hidden">
		<Header />
		<div class="flex-1 overflow-y-auto px-8 pb-8">
			{@render children()}
		</div>
	</main>
</div>
