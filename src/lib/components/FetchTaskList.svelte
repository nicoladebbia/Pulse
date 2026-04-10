<script lang="ts">
	import { stages, fetchDone, fetchError } from '$lib/stores/fetch';
	import type { StageState } from '$lib/stores/fetch';

	let { compact = false }: { compact?: boolean } = $props();

	function stageIcon(status: string): string {
		switch (status) {
			case 'completed': return 'check';
			case 'active': return 'active';
			case 'failed': return 'failed';
			default: return 'pending';
		}
	}
</script>

<div class="fetch-task-list {compact ? 'compact' : 'full'}">
	{#each $stages as stage, i (stage.id)}
		{@const icon = stageIcon(stage.status)}
		<div
			class="stage-row {stage.status}"
			class:compact
			style="--delay: {i * 30}ms"
		>
			<!-- Status icon -->
			<div class="stage-icon {stage.status}">
				{#if icon === 'check'}
					<svg class="check-svg" viewBox="0 0 16 16" fill="none">
						<path d="M3 8.5L6.5 12L13 4" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
					</svg>
				{:else if icon === 'active'}
					<div class="active-dot"></div>
				{:else if icon === 'failed'}
					<svg class="fail-svg" viewBox="0 0 16 16" fill="none">
						<path d="M4 4L12 12M12 4L4 12" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
					</svg>
				{:else}
					<div class="pending-dot"></div>
				{/if}
			</div>

			<!-- Label + detail -->
			<div class="stage-content">
				<span class="stage-label">{stage.label}</span>
				{#if stage.status === 'active' && stage.detail && !compact}
					<span class="stage-detail">{stage.detail}</span>
				{/if}
			</div>
		</div>
	{/each}

	{#if $fetchError}
		<div class="error-row">
			<span class="text-rose-400 text-xs">{$fetchError}</span>
		</div>
	{/if}
</div>

<style>
	.fetch-task-list {
		display: flex;
		flex-direction: column;
	}

	.fetch-task-list.full {
		gap: 2px;
	}

	.fetch-task-list.compact {
		gap: 1px;
	}

	/* --- Stage row --- */
	.stage-row {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 5px 0;
		transition: all 0.4s cubic-bezier(0.4, 0, 0.2, 1);
	}

	.stage-row.compact {
		gap: 8px;
		padding: 3px 0;
	}

	.stage-row.completed {
		opacity: 0.5;
	}

	.stage-row.completed .stage-label {
		text-decoration: line-through;
		text-decoration-color: var(--color-text-muted);
	}

	.stage-row.active {
		opacity: 1;
	}

	.stage-row.pending {
		opacity: 0.35;
	}

	/* --- Icons --- */
	.stage-icon {
		width: 18px;
		height: 18px;
		display: flex;
		align-items: center;
		justify-content: center;
		flex-shrink: 0;
	}

	.compact .stage-icon {
		width: 14px;
		height: 14px;
	}

	.check-svg {
		width: 14px;
		height: 14px;
		color: #22c55e;
		animation: checkIn 0.35s cubic-bezier(0.34, 1.56, 0.64, 1) forwards;
	}

	.compact .check-svg {
		width: 11px;
		height: 11px;
	}

	@keyframes checkIn {
		0% { transform: scale(0); opacity: 0; }
		50% { transform: scale(1.3); opacity: 1; }
		100% { transform: scale(1); opacity: 1; }
	}

	.active-dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--color-ai);
		animation: activePulse 1.5s ease-in-out infinite;
		box-shadow: 0 0 8px var(--color-ai);
	}

	.compact .active-dot {
		width: 6px;
		height: 6px;
	}

	@keyframes activePulse {
		0%, 100% { transform: scale(1); opacity: 1; box-shadow: 0 0 4px var(--color-ai); }
		50% { transform: scale(1.3); opacity: 0.8; box-shadow: 0 0 12px var(--color-ai); }
	}

	.pending-dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		border: 1.5px solid var(--color-text-muted);
		opacity: 0.4;
	}

	.compact .pending-dot {
		width: 5px;
		height: 5px;
		border-width: 1px;
	}

	.fail-svg {
		width: 14px;
		height: 14px;
		color: #f43f5e;
	}

	/* --- Content --- */
	.stage-content {
		display: flex;
		flex-direction: column;
		gap: 1px;
		min-width: 0;
	}

	.stage-label {
		font-size: 12px;
		color: var(--color-text-secondary);
		transition: color 0.3s ease;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.compact .stage-label {
		font-size: 11px;
	}

	.stage-row.active .stage-label {
		color: var(--color-text);
		font-weight: 500;
	}

	.stage-row.completed .stage-label {
		color: var(--color-text-muted);
	}

	.stage-detail {
		font-size: 10px;
		color: var(--color-text-muted);
		font-family: var(--font-mono);
		animation: detailFade 0.25s ease-out;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	@keyframes detailFade {
		from { opacity: 0; transform: translateY(4px); }
		to { opacity: 1; transform: translateY(0); }
	}

	.error-row {
		padding: 4px 0 4px 28px;
	}
</style>
