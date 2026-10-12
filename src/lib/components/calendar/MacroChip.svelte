<script lang="ts">
	import type { MacroKind } from '$lib/tauri/types';
	import { MACRO_LABEL } from './calendar-utils';
	let { kind }: { kind: MacroKind } = $props();
</script>

<span class="inline-block text-[10px] font-semibold tracking-wide rounded px-1.5 py-0.5 bg-ai-dim text-blue-300 border border-ai/30">
	{MACRO_LABEL[kind] ?? kind}
</span>
