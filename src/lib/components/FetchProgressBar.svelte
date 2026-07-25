<script lang="ts">
	import { fetchProgress, isFetching, lastStatus, lastReason, lastAt, fetchEta } from '$lib/stores/fetch';

	// Ticking clock so "2h ago" stays fresh without a reload.
	let now = $state(Date.now());
	$effect(() => {
		const h = setInterval(() => { now = Date.now(); }, 30_000);
		return () => clearInterval(h);
	});

	function timeAgo(iso: string | null, nowMs: number): string {
		if (!iso) return '';
		const ts = new Date(iso).getTime();
		if (Number.isNaN(ts)) return '';
		const diffMin = Math.floor((nowMs - ts) / 60_000);
		if (diffMin < 1) return 'just now';
		if (diffMin < 60) return `${diffMin}m ago`;
		const diffHr = Math.floor(diffMin / 60);
		if (diffHr < 24) return `${diffHr}h ago`;
		return `${Math.floor(diffHr / 24)}d ago`;
	}

	function formatEta(secs: number): string {
		if (secs < 60) return '<1 min';
		return `~${Math.round(secs / 60)} min`;
	}

	// Percent shown on the bar. Fall back to a small non-zero value while starting so the
	// bar is visibly "moving" even before the first weighted stage lands.
	let percent = $derived(
		$isFetching ? Math.max($fetchProgress?.percent ?? 0, 2) : 100
	);
	let stageLabel = $derived($fetchProgress?.stage_label ?? 'Working…');
	let detail = $derived($fetchProgress?.detail ?? null);
	let eta = $derived($fetchEta.eta_secs);

	let relativeAt = $derived(timeAgo($lastAt, now));

	// Build the failure line as one string so whitespace between segments is preserved
	// (Svelte trims spaces adjacent to {#if} blocks in markup).
	let failedLine = $derived(
		[
			`Last fetch ${$lastStatus === 'interrupted' ? 'interrupted' : 'failed'}`,
			relativeAt ? ` ${relativeAt}` : '',
			$lastReason ? `: ${$lastReason}` : '',
		].join('')
	);
</script>

<div class="progress-wrap">
	{#if $isFetching}
		<!-- RUNNING: live bar + stage + elapsed/ETA -->
		<div class="running">
			<div class="track">
				<div class="fill" style="width: {percent}%"></div>
			</div>
			<div class="running-meta">
				<span class="stage" title={detail ?? stageLabel}>{stageLabel}</span>
				<span class="pct">{percent}%{#if eta != null && eta > 0} · {formatEta(eta)}{/if}</span>
			</div>
			{#if detail}
				<div class="detail" title={detail}>{detail}</div>
			{/if}
		</div>
	{:else if $lastStatus === 'failed' || $lastStatus === 'interrupted'}
		<!-- FAILED / INTERRUPTED: durable red line, works for scheduled runs too -->
		<div class="failed" title={$lastReason ?? ''}>
			<span class="dot"></span>
			<span class="msg">{failedLine}</span>
		</div>
	{:else}
		<!-- IDLE / COMPLETE: always show SOMETHING (user: "I always want to see it") -->
		<div class="idle">
			<span class="dot idle-dot"></span>
			<span class="msg">{relativeAt ? `Updated ${relativeAt}` : 'No fetch yet today'}</span>
		</div>
	{/if}
</div>

<style>
	.progress-wrap {
		width: 100%;
		margin-bottom: 8px;
	}

	/* --- Running --- */
	.track {
		width: 100%;
		height: 4px;
		border-radius: 3px;
		background: var(--color-bg-card);
		overflow: hidden;
	}
	.fill {
		height: 100%;
		border-radius: 3px;
		background: linear-gradient(90deg, var(--color-ai), #8b5cf6);
		box-shadow: 0 0 8px var(--color-ai);
		transition: width 0.5s cubic-bezier(0.4, 0, 0.2, 1);
		background-size: 200% 100%;
		animation: shimmer 2s linear infinite;
	}
	@keyframes shimmer {
		0% { background-position: 200% 0; }
		100% { background-position: -200% 0; }
	}
	.running-meta {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-top: 5px;
		font-size: 11px;
	}
	.stage {
		color: var(--color-text);
		font-weight: 500;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.pct {
		color: var(--color-text-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		flex-shrink: 0;
		margin-left: 8px;
	}
	.detail {
		margin-top: 2px;
		font-size: 10px;
		color: var(--color-text-muted);
		font-family: var(--font-mono);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* --- Failed / Interrupted --- */
	.failed {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 11px;
		color: #f43f5e;
		line-height: 1.3;
	}
	.failed .dot {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: #f43f5e;
		flex-shrink: 0;
		box-shadow: 0 0 6px #f43f5e;
	}
	.failed .msg {
		overflow: hidden;
		text-overflow: ellipsis;
		display: -webkit-box;
		-webkit-line-clamp: 2;
		line-clamp: 2;
		-webkit-box-orient: vertical;
	}

	/* --- Idle / Complete --- */
	.idle {
		display: flex;
		align-items: center;
		gap: 6px;
		font-size: 10px;
		color: var(--color-text-muted);
	}
	.idle-dot {
		width: 5px;
		height: 5px;
		border-radius: 50%;
		background: #22c55e;
		opacity: 0.6;
		flex-shrink: 0;
	}
</style>
