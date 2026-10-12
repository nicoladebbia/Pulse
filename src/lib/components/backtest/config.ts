// Form state for the backtest page and the mapping to the backend's BacktestConfig.
import type { BacktestConfig } from '$lib/tauri/types';

export type ExitKind = 'fixed' | 'atr';
export type SizingKind = 'fixed' | 'risk' | 'tiers';

export interface FormState {
	start: string;
	end: string;
	minScore: number | null;
	maxPositions: number | null;
	maxHold: number | null;
	exitKind: ExitKind;
	stopLoss: number | null; // negative percent
	takeProfit: number | null;
	atrMult: number | null;
	hardStop: number | null; // percent, positive
	activate: number | null; // percent
	sizingKind: SizingKind;
	sizePct: number | null;
	riskPerTrade: number | null; // percent
	maxHeat: number | null; // percent
	maxTicker: number | null; // percent
}

export function liveBotForm(start: string, end: string): FormState {
	return {
		start, end, minScore: 0.4, maxPositions: 10, maxHold: 60,
		exitKind: 'atr', stopLoss: -10, takeProfit: 15, atrMult: 3, hardStop: 15, activate: 0,
		sizingKind: 'risk', sizePct: 5, riskPerTrade: 0.5, maxHeat: 10, maxTicker: 8
	};
}

export function simpleForm(start: string, end: string): FormState {
	return {
		start, end, minScore: 0.4, maxPositions: 10, maxHold: 90,
		exitKind: 'fixed', stopLoss: -10, takeProfit: 15, atrMult: 3, hardStop: 15, activate: 0,
		sizingKind: 'fixed', sizePct: 5, riskPerTrade: 0.5, maxHeat: 10, maxTicker: 8
	};
}

const num = (v: number | null | undefined): v is number => typeof v === 'number' && Number.isFinite(v);

function check(errors: Record<string, string>, key: string, v: number | null, lo: number, hi: number, label: string, int = false) {
	if (!num(v)) errors[key] = `${label} is required`;
	else if (v < lo || v > hi) errors[key] = `${label} must be between ${lo} and ${hi}`;
	else if (int && !Number.isInteger(v)) errors[key] = `${label} must be a whole number`;
}

export function validate(f: FormState): Record<string, string> {
	const e: Record<string, string> = {};
	if (!f.start) e.start = 'Pick a start date';
	if (!f.end) e.end = 'Pick an end date';
	if (f.start && f.end && f.start >= f.end) e.end = 'End date must be after the start date';
	check(e, 'minScore', f.minScore, 0.1, 1, 'Min score');
	check(e, 'maxPositions', f.maxPositions, 1, 50, 'Max positions', true);
	check(e, 'maxHold', f.maxHold, 1, 365, 'Max hold', true);
	if (f.exitKind === 'fixed') {
		if (!num(f.stopLoss)) e.stopLoss = 'Stop loss is required';
		else if (f.stopLoss >= 0 || f.stopLoss < -100) e.stopLoss = 'Stop loss must be negative, down to -100';
		check(e, 'takeProfit', f.takeProfit, 0.1, 1000, 'Take profit');
	} else {
		check(e, 'atrMult', f.atrMult, 0.5, 20, 'ATR multiple');
		check(e, 'hardStop', f.hardStop, 1, 100, 'Hard stop');
		check(e, 'activate', f.activate, 0, 1000, 'Activate-after gain');
	}
	if (f.sizingKind === 'fixed') check(e, 'sizePct', f.sizePct, 0.5, 100, 'Position size');
	if (f.sizingKind === 'risk') {
		check(e, 'riskPerTrade', f.riskPerTrade, 0.05, 10, 'Risk per trade');
		check(e, 'maxHeat', f.maxHeat, 0.1, 100, 'Max total risk');
		check(e, 'maxTicker', f.maxTicker, 0.5, 100, 'Max per ticker');
	}
	return e;
}

const r = (v: number, d = 6) => Number(v.toFixed(d));

/** Assumes validate() returned no errors. */
export function toConfig(f: FormState): BacktestConfig {
	const cfg: BacktestConfig = {
		start_date: f.start,
		end_date: f.end,
		min_score: f.minScore as number,
		max_positions: f.maxPositions as number,
		max_hold_days: f.maxHold as number,
		// The backend requires these even when the chosen exit/sizing model ignores them.
		stop_loss_pct: f.stopLoss ?? -10,
		take_profit_pct: f.takeProfit ?? 15,
		position_size_pct: f.sizePct ?? 5,
		exit_model:
			f.exitKind === 'atr'
				? { AtrTrail: { atr_mult: f.atrMult as number, hard_stop_pct: f.hardStop as number, activate_pct: f.activate as number } }
				: 'FixedPct'
	};
	if (f.sizingKind === 'risk') {
		cfg.risk_sizing = {
			risk_per_trade: r((f.riskPerTrade as number) / 100),
			max_heat: r((f.maxHeat as number) / 100),
			max_ticker_pct: r((f.maxTicker as number) / 100)
		};
	}
	if (f.sizingKind === 'tiers') cfg.use_live_tiers = true;
	return cfg;
}

export const EXIT_LABELS: Record<string, string> = {
	take_profit: 'Take profit',
	stop_loss: 'Stop loss',
	trailing_stop: 'Trailing stop',
	max_hold: 'Max hold reached',
	data_end: 'Still open at end of data'
};
export const exitLabel = (r: string) => EXIT_LABELS[r] ?? r.replace(/_/g, ' ');
