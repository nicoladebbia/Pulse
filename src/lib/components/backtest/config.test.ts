import { describe, expect, it } from 'vitest';
import { exitLabel, liveBotForm, simpleForm, toConfig, validate } from './config';

const S = '2026-05-01';
const E = '2026-10-01';

describe('backtest form', () => {
	it('the live-bot preset is valid and sends the live exit and risk sizing', () => {
		const f = liveBotForm(S, E);
		expect(validate(f)).toEqual({});
		const c = toConfig(f);
		expect(c.exit_model).toEqual({ AtrTrail: { atr_mult: 3, hard_stop_pct: 15, activate_pct: 0 } });
		expect(c.risk_sizing).toEqual({ risk_per_trade: 0.005, max_heat: 0.1, max_ticker_pct: 0.08 });
		expect(c.use_live_tiers).toBeUndefined();
		expect([c.min_score, c.max_positions, c.max_hold_days]).toEqual([0.4, 10, 60]);
	});

	it('the simple preset sends fixed exits and neither risk sizing nor tiers', () => {
		const f = simpleForm(S, E);
		expect(validate(f)).toEqual({});
		const c = toConfig(f);
		expect(c.exit_model).toBe('FixedPct');
		expect(c.risk_sizing).toBeUndefined();
		expect(c.use_live_tiers).toBeUndefined();
		expect([c.stop_loss_pct, c.take_profit_pct, c.position_size_pct]).toEqual([-10, 15, 5]);
	});

	it('score tiers set the flag only', () => {
		const c = toConfig({ ...simpleForm(S, E), sizingKind: 'tiers' });
		expect(c.use_live_tiers).toBe(true);
		expect(c.risk_sizing).toBeUndefined();
	});

	it('percent inputs become fractions without float noise', () => {
		const c = toConfig({ ...liveBotForm(S, E), riskPerTrade: 0.7, maxHeat: 12.3, maxTicker: 4.1 });
		expect(c.risk_sizing).toEqual({ risk_per_trade: 0.007, max_heat: 0.123, max_ticker_pct: 0.041 });
	});

	it('rejects an end date that is not after the start', () => {
		expect(validate(simpleForm(E, S)).end).toMatch(/after/);
		expect(validate(simpleForm(S, S)).end).toMatch(/after/);
		expect(validate(simpleForm('', E)).start).toBeTruthy();
	});

	it('rejects empty, out of range and fractional numbers', () => {
		const f = simpleForm(S, E);
		expect(validate({ ...f, minScore: null }).minScore).toMatch(/required/);
		expect(validate({ ...f, minScore: 2 }).minScore).toMatch(/between/);
		expect(validate({ ...f, maxPositions: 2.5 }).maxPositions).toMatch(/whole/);
		expect(validate({ ...f, maxHold: 0 }).maxHold).toBeTruthy();
		expect(validate({ ...f, minScore: NaN }).minScore).toMatch(/required/);
	});

	it('checks only the fields of the chosen exit and sizing model', () => {
		const fixed = { ...simpleForm(S, E), atrMult: null, hardStop: null, riskPerTrade: null };
		expect(validate(fixed)).toEqual({});
		const atr = { ...liveBotForm(S, E), stopLoss: null, takeProfit: null, sizePct: null };
		expect(validate(atr)).toEqual({});
		expect(validate({ ...simpleForm(S, E), stopLoss: 5 }).stopLoss).toMatch(/negative/);
		expect(validate({ ...liveBotForm(S, E), atrMult: null }).atrMult).toMatch(/required/);
	});

	it('labels the backend exit reasons', () => {
		expect(exitLabel('trailing_stop')).toBe('Trailing stop');
		expect(exitLabel('data_end')).toBe('Still open at end of data');
		expect(exitLabel('some_new_reason')).toBe('some new reason');
	});
});
