<script lang="ts">
	import { isTauri, mockResearchPapers, mockResearchStats, mockResearchDetail, mockProposalBacktest } from '$lib/tauri/mock';
	import {
		getResearchPapers, getResearchStats, getResearchPaper, queuePaperRead,
		backtestProposal, setProposalStatus
	} from '$lib/tauri/commands';
	import type {
		ResearchPaperRow, ResearchStats, ResearchPaperDetail, ResearchProposal, WhatIfSummary
	} from '$lib/tauri/types';
	import katex from 'katex';
	import 'katex/dist/katex.min.css';

	let papers = $state<ResearchPaperRow[]>([]);
	let stats = $state<ResearchStats | null>(null);
	let filter = $state<'read' | 'queued' | 'skipped' | 'all'>('read');
	let loading = $state(true);
	let error = $state<string | null>(null);
	let openId = $state<number | null>(null);
	let detail = $state<ResearchPaperDetail | null>(null);
	let detailLoading = $state(false);
	let busyProposal = $state<number | null>(null);
	let hypothesesTested = $state<number | null>(null);
	let notice = $state<string | null>(null);
	let loaded = false;

	$effect(() => {
		if (loaded) return;
		loaded = true;
		load();
	});

	async function load() {
		loading = true;
		error = null;
		if (!isTauri()) {
			papers = mockResearchPapers;
			stats = mockResearchStats;
			loading = false;
			return;
		}
		try {
			[papers, stats] = await Promise.all([getResearchPapers(400), getResearchStats()]);
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	const visible = $derived(
		papers.filter((p) =>
			filter === 'all' ? true
			: filter === 'read' ? p.status === 'read'
			: filter === 'queued' ? ['triaged', 'reading', 'failed', 'new'].includes(p.status)
			: p.status === 'skipped'
		)
	);

	async function toggle(p: ResearchPaperRow) {
		notice = null;
		if (openId === p.id) {
			openId = null;
			detail = null;
			return;
		}
		openId = p.id;
		detail = null;
		detailLoading = true;
		if (!isTauri()) {
			detail = { ...mockResearchDetail, paper: p };
			detailLoading = false;
			return;
		}
		try {
			detail = await getResearchPaper(p.id);
		} catch (e) {
			notice = `Could not load paper: ${e}`;
		} finally {
			detailLoading = false;
		}
	}

	async function readAnyway(p: ResearchPaperRow) {
		try {
			await queuePaperRead(p.id);
			notice = 'Queued. The fetcher reads it on its next hourly run (counts against the research spend cap).';
			await load();
			if (detail) detail.paper.status = 'triaged';
		} catch (e) {
			notice = `Could not queue: ${e}`;
		}
	}

	async function runBacktest(pr: ResearchProposal) {
		busyProposal = pr.id;
		notice = null;
		try {
			const r = isTauri() ? await backtestProposal(pr.id) : mockProposalBacktest;
			pr.result = r.result;
			pr.tested_at = new Date().toISOString();
			if (pr.status === 'proposed') pr.status = 'tested';
			hypothesesTested = r.hypotheses_tested;
		} catch (e) {
			notice = `Backtest failed: ${e}`;
		} finally {
			busyProposal = null;
		}
	}

	async function decide(pr: ResearchProposal, status: 'kept' | 'rejected') {
		try {
			if (isTauri()) await setProposalStatus(pr.id, status);
			pr.status = status;
		} catch (e) {
			notice = `Could not save decision: ${e}`;
		}
	}

	function copy(text: string) {
		navigator.clipboard.writeText(text).then(
			() => (notice = 'Spec copied — paste it into a Claude Code session to build it.'),
			() => (notice = 'Clipboard unavailable')
		);
	}

	function tex(src: string, display = false): string {
		try {
			return katex.renderToString(src, { displayMode: display, throwOnError: false, strict: 'ignore' });
		} catch {
			return `<code>${src.replace(/</g, '&lt;')}</code>`;
		}
	}

	/** Render inline `$...$` spans inside prose; everything else is escaped text. */
	function prose(text: string): string {
		const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
		return text
			.split(/(\$[^$\n]+\$)/g)
			.map((part) => (part.length > 2 && part.startsWith('$') && part.endsWith('$') ? tex(part.slice(1, -1)) : esc(part)))
			.join('');
	}

	const scoreClass = (s: number | null) =>
		s === null ? 'bg-zinc-500/15 text-text-muted'
		: s >= 8 ? 'bg-emerald-500/15 text-emerald-400'
		: s >= 6 ? 'bg-ai/15 text-ai'
		: s >= 3 ? 'bg-amber-500/10 text-amber-400'
		: 'bg-zinc-500/10 text-text-muted';

	const statusLabel: Record<string, string> = {
		new: 'awaiting triage', triaged: 'queued to read', reading: 'reading…', read: 'read',
		skipped: 'skipped', failed: 'failed'
	};

	const verdictClass: Record<string, string> = {
		better: 'bg-emerald-500/15 text-emerald-400 border-emerald-500/30',
		worse: 'bg-rose-500/15 text-rose-400 border-rose-500/30',
		mixed: 'bg-amber-500/10 text-amber-400 border-amber-500/30',
		inconclusive: 'bg-zinc-500/10 text-text-secondary border-border'
	};

	const sevClass: Record<string, string> = {
		high: 'text-rose-400', medium: 'text-amber-400', low: 'text-text-muted'
	};

	const pct = (v: number, d = 1) => `${v >= 0 ? '' : ''}${v.toFixed(d)}%`;
	const deltaEntries = (d: Record<string, unknown> | null) =>
		d ? Object.entries(d).filter(([, v]) => v !== null && v !== undefined) : [];
	const fmtDelta = (v: unknown) =>
		Array.isArray(v)
			? v.map((x: any) => ('dimension' in x ? `${x.dimension}=${x.weight}` : `>${x.above_score}: ${x.pct}%`)).join(', ')
			: String(v);

	const metricRows: { key: keyof WhatIfSummary; label: string; fmt: (v: number) => string; higherIsBetter: boolean | null }[] = [
		{ key: 'trades', label: 'Closed trades', fmt: (v) => String(v), higherIsBetter: null }, // a count, not a quality
		{ key: 'hit_rate', label: 'Hit rate', fmt: (v) => pct(v, 0), higherIsBetter: true },
		{ key: 'total_return_pct', label: 'Total return', fmt: (v) => pct(v, 2), higherIsBetter: true },
		{ key: 'max_drawdown_pct', label: 'Max drawdown', fmt: (v) => pct(v, 2), higherIsBetter: false },
		{ key: 'sharpe', label: 'Sharpe', fmt: (v) => v.toFixed(2), higherIsBetter: true }
	];
</script>

<div>
	<!-- Stats -->
	{#if stats}
		<div class="grid grid-cols-5 gap-3 mb-4">
			{#each [
				{ label: 'Read', value: String(stats.read) },
				{ label: 'Queued', value: String(stats.queued) },
				{ label: 'Skipped', value: String(stats.skipped) },
				{ label: 'Proposals tested', value: `${stats.tested}/${stats.proposals}` },
				{ label: 'Spend (30d)', value: `$${stats.spent_30d_usd.toFixed(2)}` }
			] as s}
				<div class="bg-bg-card border border-border rounded-xl p-3">
					<div class="text-[10px] text-text-muted uppercase tracking-wider">{s.label}</div>
					<div class="text-lg font-mono font-bold text-text">{s.value}</div>
				</div>
			{/each}
		</div>
	{/if}

	<!-- Filter -->
	<div class="flex items-center gap-2 mb-4">
		{#each [
			{ id: 'read', label: 'Read' },
			{ id: 'queued', label: 'Queued' },
			{ id: 'skipped', label: 'Skipped' },
			{ id: 'all', label: 'All' }
		] as f}
			<button
				class="px-3 py-1 text-xs rounded-full border transition-colors {filter === f.id ? 'bg-bg-card border-ai/40 text-text' : 'border-border text-text-muted hover:text-text'}"
				onclick={() => (filter = f.id as typeof filter)}
			>{f.label}</button>
		{/each}
		<span class="text-[11px] text-text-muted ml-auto">arXiv q-fin + finance ML · triaged by relevance to this system · ≥6/10 read in full</span>
	</div>

	{#if notice}
		<div class="mb-3 px-3 py-2 rounded-lg text-xs bg-ai/10 text-ai border border-ai/20">{notice}</div>
	{/if}

	{#if loading}
		<div class="flex justify-center py-16">
			<div class="w-6 h-6 border-2 border-ai border-t-transparent rounded-full animate-spin"></div>
		</div>
	{:else if error}
		<div class="text-center py-16 text-rose-400 text-sm">{error}</div>
	{:else if visible.length === 0}
		<div class="text-center py-16 text-text-muted text-sm">
			{#if papers.length === 0}
				No papers yet. The fetcher pulls arXiv every 6 hours and reads the relevant ones on its hourly runs.
			{:else}
				Nothing in this view.
			{/if}
		</div>
	{:else}
		<div class="space-y-2">
			{#each visible as p (p.id)}
				<div class="bg-bg-card border border-border rounded-xl overflow-hidden">
					<button class="w-full text-left px-4 py-3 flex gap-3 items-start hover:bg-bg/40" onclick={() => toggle(p)}>
						<span class="shrink-0 mt-0.5 w-9 text-center text-xs font-mono font-bold rounded-md py-0.5 {scoreClass(p.triage_score)}">
							{p.triage_score ?? '–'}
						</span>
						<div class="min-w-0 flex-1">
							<div class="text-sm text-text font-medium leading-snug">{p.title}</div>
							<div class="text-xs text-text-muted mt-1 line-clamp-2">
								{p.one_line ?? p.triage_reason ?? p.authors}
							</div>
						</div>
						<div class="shrink-0 text-right">
							<div class="text-[11px] text-text-muted">{p.published_at.slice(0, 10)}</div>
							<div class="text-[10px] mt-0.5 {p.status === 'read' ? 'text-emerald-400' : p.status === 'failed' ? 'text-rose-400' : 'text-text-muted'}">
								{statusLabel[p.status] ?? p.status}{p.proposal_count > 0 ? ` · ${p.proposal_count} proposal${p.proposal_count > 1 ? 's' : ''}` : ''}
							</div>
						</div>
					</button>

					{#if openId === p.id}
						<div class="border-t border-border px-5 py-4 text-sm">
							{#if detailLoading}
								<div class="text-text-muted text-xs">Loading…</div>
							{:else if detail}
								<div class="flex flex-wrap gap-3 text-xs text-text-muted mb-4">
									<a class="text-ai hover:underline" href={`https://arxiv.org/abs/${p.arxiv_id}`} target="_blank" rel="noreferrer">arXiv {p.arxiv_id}</a>
									<a class="text-ai hover:underline" href={`https://arxiv.org/pdf/${p.arxiv_id}`} target="_blank" rel="noreferrer">PDF</a>
									{#if p.pages}<span>{p.pages} pages</span>{/if}
									<span>{p.categories}</span>
									{#if detail.read_model}<span>read by {detail.read_model} from {detail.text_source} · ${detail.read_cost_usd?.toFixed(3)}</span>{/if}
								</div>
								<div class="text-xs text-text-secondary mb-4">{p.authors}</div>

								{#if detail.study}
									{@const s = detail.study}
									<p class="text-base text-text leading-relaxed mb-4">{@html prose(s.one_line)}</p>

									<div class="flex items-center gap-2 mb-5">
										<span class="text-[10px] uppercase tracking-wider text-text-muted">Evidence</span>
										<span class="text-xs px-2 py-0.5 rounded-full border {s.evidence_strength === 'strong' ? verdictClass.better : s.evidence_strength === 'weak' ? verdictClass.worse : verdictClass.mixed}">{s.evidence_strength}</span>
									</div>

									{#each [
										{ h: 'The problem', t: s.problem },
										{ h: 'How it works', t: s.method }
									] as sec}
										<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-1.5">{sec.h}</h4>
										<div class="text-text-secondary leading-relaxed whitespace-pre-line">{@html prose(sec.t)}</div>
									{/each}

									{#if s.key_equations.length}
										<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-2">Key equations</h4>
										<div class="space-y-3">
											{#each s.key_equations as eq}
												<div class="bg-bg rounded-lg px-4 py-3 border border-border">
													<div class="overflow-x-auto text-text">{@html tex(eq.latex, true)}</div>
													<div class="text-xs text-text-secondary mt-2 leading-relaxed">{@html prose(eq.meaning)}</div>
												</div>
											{/each}
										</div>
									{/if}

									<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-1.5">Data</h4>
									<div class="text-text-secondary leading-relaxed">{@html prose(s.data)}</div>

									{#if s.results.length}
										<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-2">Results</h4>
										<table class="w-full text-xs">
											<tbody>
												{#each s.results as r}
													<tr class="border-b border-border/50 align-top">
														<td class="py-1.5 pr-3 text-text-secondary">{@html prose(r.claim)}</td>
														<td class="py-1.5 pr-3 font-mono text-text whitespace-nowrap">{@html prose(r.number)}</td>
														<td class="py-1.5 text-text-muted whitespace-nowrap">{r.where}</td>
													</tr>
												{/each}
											</tbody>
										</table>
									{/if}

									<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-1.5">Robustness</h4>
									<div class="text-text-secondary leading-relaxed">{@html prose(s.robustness)}</div>

									<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-2">Critique</h4>
									{#if !s.critique.length}
										<div class="text-xs text-text-muted">No methodological flaws raised by the reader. Weigh that against the evidence rating, not as a clean bill of health.</div>
									{:else}
										<ul class="space-y-1.5">
											{#each s.critique as c}
												<li class="text-xs leading-relaxed">
													<span class="font-mono {sevClass[c.severity]}">{c.severity}</span>
													<span class="text-text-muted"> · {c.issue.replace(/_/g, ' ')} · </span>
													<span class="text-text-secondary">{@html prose(c.detail)}</span>
												</li>
											{/each}
										</ul>
									{/if}

									<h4 class="text-xs uppercase tracking-wider text-text-muted mt-5 mb-1.5">What transfers to Pulse</h4>
									<div class="text-text-secondary leading-relaxed">{@html prose(s.transfers_to_pulse)}</div>

									{#if s.glossary.length}
										<details class="mt-5">
											<summary class="text-xs uppercase tracking-wider text-text-muted cursor-pointer">Glossary ({s.glossary.length})</summary>
											<dl class="mt-2 space-y-1.5 text-xs">
												{#each s.glossary as g}
													<div><dt class="inline text-text font-medium">{@html prose(g.term)}</dt> <dd class="inline text-text-secondary">— {@html prose(g.definition)}</dd></div>
												{/each}
											</dl>
										</details>
									{/if}
								{:else}
									<h4 class="text-xs uppercase tracking-wider text-text-muted mb-1.5">Abstract</h4>
									<p class="text-text-secondary leading-relaxed mb-3">{@html prose(detail.abstract_text)}</p>
									{#if detail.triage}
										<p class="text-xs text-text-muted"><span class="text-text-secondary">Triage {detail.triage.relevance}/10:</span> {detail.triage.reason}</p>
									{/if}
									{#if p.error}
										<p class="text-xs text-rose-400 mt-2">{p.error}</p>
									{/if}
									{#if p.status === 'skipped' || p.status === 'failed'}
										<button class="mt-3 px-3 py-1.5 text-xs rounded-lg border border-ai/40 text-ai hover:bg-ai/10" onclick={() => readAnyway(p)}>
											Read anyway
										</button>
									{:else if p.status === 'triaged' || p.status === 'reading'}
										<p class="text-xs text-text-muted mt-2">Full read in progress — results arrive on the fetcher's next hourly run.</p>
									{/if}
								{/if}

								<!-- Proposals -->
								{#if detail.proposals.length}
									<h4 class="text-xs uppercase tracking-wider text-text-muted mt-6 mb-2">Proposals for Pulse</h4>
									<div class="space-y-3">
										{#each detail.proposals as pr (pr.id)}
											<div class="rounded-lg border border-border bg-bg px-4 py-3">
												<div class="flex items-start gap-2">
													<span class="text-[10px] font-mono uppercase px-1.5 py-0.5 rounded {pr.kind === 'param' ? 'bg-ai/15 text-ai' : 'bg-zinc-500/15 text-text-secondary'}">{pr.kind === 'param' ? 'testable' : pr.kind.replace('_', ' ')}</span>
													<div class="flex-1">
														<div class="text-text font-medium text-sm">{pr.title}</div>
														<div class="text-[11px] text-text-muted">{pr.component} · {pr.status}</div>
													</div>
												</div>
												<p class="text-xs text-text-secondary mt-2 leading-relaxed">{@html prose(pr.rationale)}</p>
												<p class="text-xs text-text-muted mt-1.5"><span class="text-text-secondary">Would be wrong if:</span> {@html prose(pr.falsifier)}</p>

												{#if pr.kind === 'param'}
													<div class="mt-2 text-[11px] font-mono text-text-secondary">
														{#each deltaEntries(pr.delta) as [k, v]}
															<span class="inline-block mr-3">{k}: <span class="text-text">{fmtDelta(v)}</span></span>
														{/each}
													</div>
													<div class="flex gap-2 mt-3">
														<button
															class="px-3 py-1.5 text-xs rounded-lg bg-ai/15 text-ai hover:bg-ai/25 disabled:opacity-50"
															disabled={busyProposal === pr.id}
															onclick={() => runBacktest(pr)}
														>{busyProposal === pr.id ? 'Backtesting…' : pr.result ? 'Re-run backtest' : 'Backtest vs live'}</button>
														{#if pr.result}
															<button class="px-3 py-1.5 text-xs rounded-lg border border-border text-text-secondary hover:text-text" onclick={() => decide(pr, 'kept')}>Keep</button>
															<button class="px-3 py-1.5 text-xs rounded-lg border border-border text-text-secondary hover:text-text" onclick={() => decide(pr, 'rejected')}>Reject</button>
														{/if}
													</div>

													{#if pr.result}
														{@const r = pr.result}
														<div class="mt-3 rounded-lg border px-3 py-2 {verdictClass[r.verdict]}">
															<span class="text-xs font-semibold uppercase">{r.verdict}</span>
															<span class="text-xs"> — {r.verdict_reason}</span>
														</div>
														<table class="w-full text-xs mt-2 font-mono">
															<thead>
																<tr class="text-text-muted text-[10px] uppercase">
																	<th class="text-left font-normal py-1"></th>
																	<th class="text-right font-normal">Train live</th>
																	<th class="text-right font-normal">Train variant</th>
																	<th class="text-right font-normal">Holdout live</th>
																	<th class="text-right font-normal">Holdout variant</th>
																</tr>
															</thead>
															<tbody>
																{#each metricRows as m}
																	{@const hb = r.holdout.baseline[m.key]}
																	{@const hv = r.holdout.variant[m.key]}
																	<tr class="border-t border-border/40">
																		<td class="py-1 text-text-muted font-sans">{m.label}</td>
																		<td class="text-right text-text-secondary">{m.fmt(r.train.baseline[m.key])}</td>
																		<td class="text-right text-text-secondary">{m.fmt(r.train.variant[m.key])}</td>
																		<td class="text-right text-text">{m.fmt(hb)}</td>
																		<td class="text-right {hv === hb || m.higherIsBetter === null ? 'text-text' : (hv > hb) === m.higherIsBetter ? 'text-emerald-400' : 'text-rose-400'}">{m.fmt(hv)}</td>
																	</tr>
																{/each}
															</tbody>
														</table>
														<div class="text-[11px] text-text-muted mt-2 leading-relaxed">
															Train {r.train_window[0]} → {r.train_window[1]} · holdout {r.holdout_window[0]} → {r.holdout_window[1]} · exits are the fixed-% proxy, not the live ATR trail.
															{#if hypothesesTested !== null}
																<span class="text-amber-400">{hypothesesTested} {hypothesesTested === 1 ? 'idea' : 'ideas'} tested on this same history — the more you try, the more likely one "wins" by chance.</span>
															{/if}
														</div>
														{#if !r.fidelity.ok}
															<div class="text-[11px] text-rose-400 mt-1">Rescoring matched only {r.fidelity.rows_matched}/{r.fidelity.rows_checked} recent stored scores — weights may have changed recently; treat this result with suspicion.</div>
														{/if}
														{#each r.notes as n}
															<div class="text-[11px] text-amber-400 mt-1">{n}</div>
														{/each}
													{/if}
												{:else if pr.spec_md}
													<details class="mt-2">
														<summary class="text-xs text-text-muted cursor-pointer">Implementation spec</summary>
														<pre class="mt-2 text-[11px] text-text-secondary whitespace-pre-wrap font-mono bg-bg-card rounded p-3 border border-border">{pr.spec_md}</pre>
														<button class="mt-2 px-3 py-1.5 text-xs rounded-lg border border-border text-text-secondary hover:text-text" onclick={() => copy(`${pr.title}\n\n${pr.rationale}\n\n${pr.spec_md}`)}>Copy spec</button>
													</details>
												{/if}
											</div>
										{/each}
									</div>
								{:else if detail.study}
									<p class="text-xs text-text-muted mt-6">No change to Pulse is supported by this paper's evidence.</p>
								{/if}
							{/if}
						</div>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>
