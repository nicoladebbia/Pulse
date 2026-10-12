<script lang="ts">
	import type { BacktestRange } from '$lib/tauri/types';
	import Segmented from './Segmented.svelte';
	import NumField from './NumField.svelte';
	import { liveBotForm, simpleForm, validate, type FormState } from './config';

	let { form = $bindable(), range, running, onrun }: { form: FormState; range: BacktestRange | null; running: boolean; onrun: () => void } = $props();

	const errors = $derived(validate(form));
	const invalid = $derived(Object.keys(errors).length > 0);
	const first = $derived(range?.first_signal_date ?? '');
	const last = $derived(range?.last_price_date ?? '');

	function shift(date: string, days: number): string {
		const d = new Date(date + 'T12:00:00Z');
		d.setUTCDate(d.getUTCDate() - days);
		return d.toISOString().slice(0, 10);
	}
	function preset(kind: 'all' | '90' | '30') {
		if (!last) return;
		form.end = last;
		form.start = kind === 'all' ? first || shift(last, 365) : shift(last, Number(kind));
		if (first && form.start < first) form.start = first;
	}
	const activePreset = $derived(
		form.end !== last ? null : form.start === first ? 'all' : form.start === shift(last, 90) ? '90' : form.start === shift(last, 30) ? '30' : null
	);
	const presetBtn = 'px-2.5 py-1 text-xs rounded-md border transition-colors duration-150 focus-visible:outline-2 focus-visible:outline-ai';
	const dateCls = 'w-full bg-bg border rounded-lg px-2.5 py-1.5 text-sm font-mono tabular-nums text-text focus-visible:outline-2 focus-visible:outline-ai [color-scheme:dark]';
</script>

<section class="bg-bg-card border border-border rounded-xl p-5" aria-labelledby="bt-settings">
	<div class="flex flex-wrap items-center justify-between gap-3 mb-4">
		<h2 id="bt-settings" class="text-sm font-semibold text-text">Rules</h2>
		<div class="flex gap-1.5">
			<button type="button" onclick={() => (form = liveBotForm(form.start, form.end))} class="{presetBtn} border-border text-text-secondary hover:text-text hover:border-text-muted">Match the live bot</button>
			<button type="button" onclick={() => (form = simpleForm(form.start, form.end))} class="{presetBtn} border-border text-text-secondary hover:text-text hover:border-text-muted">Simple fixed exits</button>
		</div>
	</div>

	<div class="grid gap-x-8 gap-y-5 lg:grid-cols-2">
		<!-- Column 1: when and how many -->
		<div class="space-y-5">
			<fieldset>
				<legend class="text-xs font-medium text-text mb-2">Date range</legend>
				<div class="grid grid-cols-2 gap-3">
					<div>
						<label for="bt-start" class="block text-xs text-text-secondary mb-1">Start</label>
						<input id="bt-start" type="date" bind:value={form.start} min={first || undefined} max={last || undefined} aria-invalid={errors.start ? 'true' : undefined} class="{dateCls} {errors.start ? 'border-rose-400/60' : 'border-border'}" />
						{#if errors.start}<p class="text-[11px] text-rose-400 mt-1">{errors.start}</p>{/if}
					</div>
					<div>
						<label for="bt-end" class="block text-xs text-text-secondary mb-1">End</label>
						<input id="bt-end" type="date" bind:value={form.end} min={first || undefined} max={last || undefined} aria-invalid={errors.end ? 'true' : undefined} class="{dateCls} {errors.end ? 'border-rose-400/60' : 'border-border'}" />
						{#if errors.end}<p class="text-[11px] text-rose-400 mt-1">{errors.end}</p>{/if}
					</div>
				</div>
				<div class="flex gap-1.5 mt-2">
					{#each [['all', 'All history'], ['90', 'Last 90 days'], ['30', 'Last 30 days']] as [k, l] (k)}
						<button type="button" disabled={!range} aria-pressed={activePreset === k} onclick={() => preset(k as 'all' | '90' | '30')}
							class="{presetBtn} disabled:opacity-40 {activePreset === k ? 'border-ai/60 text-text bg-ai/10' : 'border-border text-text-secondary hover:text-text'}">{l}</button>
					{/each}
				</div>
			</fieldset>

			<div class="grid grid-cols-3 gap-3">
				<NumField id="bt-score" label="Min score" step="0.05" bind:value={form.minScore} error={errors.minScore} hint="0.1 to 1" />
				<NumField id="bt-pos" label="Max positions" bind:value={form.maxPositions} error={errors.maxPositions} hint="1 to 50" />
				<NumField id="bt-hold" label="Max hold" suffix="d" bind:value={form.maxHold} error={errors.maxHold} hint="1 to 365 days" />
			</div>
		</div>

		<!-- Column 2: exits and sizing -->
		<div class="space-y-5">
			<div>
				<p class="text-xs font-medium text-text mb-2" id="bt-exit-l">Exit rules</p>
				<Segmented label="Exit rules" bind:value={form.exitKind} options={[{ id: 'fixed', label: 'Fixed %' }, { id: 'atr', label: 'ATR trail' }]} />
				<div class="grid grid-cols-3 gap-3 mt-3">
					{#if form.exitKind === 'fixed'}
						<NumField id="bt-sl" label="Stop loss" suffix="%" bind:value={form.stopLoss} error={errors.stopLoss} />
						<NumField id="bt-tp" label="Take profit" suffix="%" bind:value={form.takeProfit} error={errors.takeProfit} />
					{:else}
						<NumField id="bt-atr" label="ATR multiple" step="0.5" suffix="x" bind:value={form.atrMult} error={errors.atrMult} />
						<NumField id="bt-hs" label="Hard stop" suffix="%" bind:value={form.hardStop} error={errors.hardStop} />
						<NumField id="bt-act" label="Activate after gain" suffix="%" bind:value={form.activate} error={errors.activate} />
					{/if}
				</div>
			</div>

			<div>
				<p class="text-xs font-medium text-text mb-2">Position sizing</p>
				<Segmented label="Position sizing" bind:value={form.sizingKind} options={[{ id: 'fixed', label: 'Fixed % of equity' }, { id: 'risk', label: 'Risk-based (live)' }, { id: 'tiers', label: 'Score tiers' }]} />
				<div class="grid grid-cols-3 gap-3 mt-3">
					{#if form.sizingKind === 'fixed'}
						<NumField id="bt-size" label="Size per position" suffix="%" bind:value={form.sizePct} error={errors.sizePct} />
					{:else if form.sizingKind === 'risk'}
						<NumField id="bt-rpt" label="Risk per trade" step="0.05" suffix="%" bind:value={form.riskPerTrade} error={errors.riskPerTrade} />
						<NumField id="bt-heat" label="Max total risk" suffix="%" bind:value={form.maxHeat} error={errors.maxHeat} />
						<NumField id="bt-tick" label="Max per ticker" suffix="%" bind:value={form.maxTicker} error={errors.maxTicker} />
					{:else}
						<p class="col-span-3 text-xs text-text-muted">Bigger positions for higher-scoring signals, using the live bot's score tiers.</p>
					{/if}
				</div>
			</div>
		</div>
	</div>

	<div class="flex items-center justify-between gap-4 mt-5 pt-4 border-t border-border-subtle">
		<p class="text-xs text-text-muted">{invalid ? 'Fix the highlighted fields to run.' : 'Press r to run.'}</p>
		<button type="button" onclick={onrun} disabled={running || invalid}
			class="px-4 py-2 rounded-lg text-sm font-medium bg-ai text-white transition-[transform,opacity] duration-150 active:scale-[0.97] disabled:opacity-40 disabled:active:scale-100 hover:brightness-110 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ai">
			{running ? 'Running…' : 'Run backtest'}
		</button>
	</div>
</section>
