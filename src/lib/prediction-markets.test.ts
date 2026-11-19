import { describe, expect, it } from 'vitest';
import { asOfLabel, changeLabel, changePoints, oddsLabel, safeMarketUrl, splitMarkets, topicLabel, volumeLabel } from './prediction-markets';
import type { PredictionMarket } from '$lib/tauri/types';

const market = (market_id: string, kind: string): PredictionMarket => ({
	market_id, kind, topic: 'economy', question: `Question ${market_id}?`, event_title: null, url: null,
	probability: 0.4, change_1d: null, change_1w: null, volume_24h: 25000, end_date: null,
});

describe('prediction market labels', () => {
	it('shows odds as whole percentages and never rounds to certain', () => {
		expect(oddsLabel(0.915)).toBe('92%');
		expect(oddsLabel(0.5)).toBe('50%');
		expect(oddsLabel(0.01)).toBe('1%');
		expect(oddsLabel(0.004)).toBe('<1%');
		expect(oddsLabel(0)).toBe('<1%');
		expect(oddsLabel(0.996)).toBe('>99%');
		expect(oddsLabel(1)).toBe('>99%');
		expect(oddsLabel(0.99)).toBe('99%');
		expect(oddsLabel(Number.NaN)).toBe('–');
	});

	it('shows a move in points with its direction', () => {
		expect(changeLabel(0.124)).toBe('+12');
		expect(changeLabel(-0.0585)).toBe('−6');
		expect(changeLabel(-0.808)).toBe('−81');
		expect(changeLabel(0.004)).toBe('');
		expect(changeLabel(-0.004)).toBe('');
		expect(changeLabel(null)).toBe('');
		expect(changeLabel(undefined)).toBe('');
		expect(changeLabel(Number.NaN)).toBe('');
		expect(Object.is(changePoints(-0.001), 0)).toBe(true);
	});

	it('shortens dollars traded', () => {
		expect(volumeLabel(1_196_452)).toBe('$1.2M');
		expect(volumeLabel(12_840_243)).toBe('$13M');
		expect(volumeLabel(504_231)).toBe('$504k');
		expect(volumeLabel(999_600)).toBe('$1M');
		expect(volumeLabel(999_400)).toBe('$999k');
		expect(volumeLabel(14_743)).toBe('$15k');
		expect(volumeLabel(900)).toBe('$900');
		expect(volumeLabel(0)).toBe('$0');
		expect(volumeLabel(-5)).toBe('$0');
		expect(volumeLabel(Number.NaN)).toBe('$0');
	});

	it('names topics, and passes an unknown one through', () => {
		expect(topicLabel('ai')).toBe('AI');
		expect(topicLabel('geopolitics')).toBe('World');
		expect(topicLabel('new_topic')).toBe('new topic');
	});

	it('says when the odds were read, by calendar day', () => {
		const now = new Date(2026, 10, 19, 7, 30);
		expect(asOfLabel('2026-11-19T02:39:50', now)).toBe('02:39');
		// Before noon the day before is still "yesterday", not "today".
		expect(asOfLabel('2026-11-18T21:02:11', now)).toBe('yesterday 21:02');
		expect(asOfLabel('2026-11-17T08:00:00', now)).toBe('Nov 17');
		expect(asOfLabel('2026-11-19 08:04:00', now)).toBe('08:04');
		expect(asOfLabel('', now)).toBe('');
		expect(asOfLabel('not a date', now)).toBe('');
	});

	it('splits the list into what moved and what is most traded, keeping the order', () => {
		const { movers, active } = splitMarkets([market('a', 'mover'), market('b', 'active'), market('c', 'mover'), market('d', 'other')]);
		expect(movers.map((m) => m.market_id)).toEqual(['a', 'c']);
		expect(active.map((m) => m.market_id)).toEqual(['b', 'd']);
		expect(splitMarkets([])).toEqual({ movers: [], active: [] });
	});

	it('opens only a link to the market site', () => {
		expect(safeMarketUrl('https://polymarket.com/event/fed-decision-in-december')).toBe('https://polymarket.com/event/fed-decision-in-december');
		for (const bad of [null, undefined, '', 'http://polymarket.com/event/x', 'https://polymarket.com.evil.io/event/x', 'https://polymarket.com/event/x?y=1', 'javascript:alert(1)', 'https://polymarket.com/event/a/b', 'file:///etc/passwd']) {
			expect(safeMarketUrl(bad)).toBeNull();
		}
	});
});
