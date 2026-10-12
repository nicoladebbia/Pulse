import SwiftUI

struct PortfolioView: View {
    @Environment(SnapshotStore.self) private var store

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        SnapshotHeader()
                        if let snapshot = store.snapshot {
                            if let account = snapshot.account {
                                if let asOf = snapshot.accountAsOf {
                                    NoticeCard(
                                        title: "Account figures are older",
                                        message: "The Mac could not reach Alpaca for the latest update. Account, positions and risk are from \(Fmt.timestamp(asOf).map { $0.formatted(date: .abbreviated, time: .shortened) } ?? asOf)."
                                    )
                                }
                                AccountCard(account: account, positionCount: snapshot.positions.count)
                            } else {
                                NoticeCard(
                                    title: "Account not in this update",
                                    message: "The Mac could not reach Alpaca when it wrote this copy, so the account and positions are missing."
                                )
                            }
                            if !snapshot.positions.isEmpty {
                                SectionTitle(text: "Positions", trailing: "\(snapshot.positions.count)")
                                ForEach(snapshot.positions.sorted { abs($0.marketValue) > abs($1.marketValue) }) { PositionRow(position: $0) }
                            }
                            if !snapshot.signals.isEmpty {
                                SectionTitle(text: "Strongest signals").id("signals")
                                Card(padding: 0) {
                                    VStack(spacing: 0) {
                                        ForEach(Array(snapshot.signals.enumerated()), id: \.element.id) { i, s in
                                            SignalRow(signal: s)
                                            if i < snapshot.signals.count - 1 {
                                                Divider().overlay(Theme.border)
                                            }
                                        }
                                    }
                                }
                            }
                            if !snapshot.recentTrades.isEmpty {
                                SectionTitle(text: "Recently closed").id("trades")
                                Card(padding: 0) {
                                    VStack(spacing: 0) {
                                        ForEach(Array(snapshot.recentTrades.enumerated()), id: \.offset) { i, t in
                                            TradeRow(trade: t)
                                            if i < snapshot.recentTrades.count - 1 {
                                                Divider().overlay(Theme.border)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 24)
                    .modifier(DebugScroll(proxy: proxy))
                }
            }
            .refreshable { await store.refresh() }
            .navigationTitle("Portfolio")
            .settingsToolbar()
            .pulseScreen()
        }
    }
}

struct AccountCard: View {
    let account: Snapshot.Account
    let positionCount: Int

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 14) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Account value").font(.footnote).foregroundStyle(Theme.textSecondary)
                    Text(Fmt.money(account.equity))
                        .font(.system(size: 38, weight: .bold, design: .rounded).monospacedDigit())
                        .foregroundStyle(Theme.text)
                    if let change = account.dayChange {
                        HStack(spacing: 6) {
                            Text(Fmt.signedMoney(change, cents: abs(change) < 100))
                            if let pct = account.dayChangePct {
                                Text("(\(Fmt.signedPct(pct, digits: 2)))")
                            }
                            Text("today").foregroundStyle(Theme.textMuted)
                        }
                        .font(.subheadline.weight(.medium).monospacedDigit())
                        .foregroundStyle(Theme.gain(change))
                    }
                }
                Divider().overlay(Theme.border)
                HStack {
                    mini("Cash", Fmt.money(account.cash))
                    Spacer()
                    mini("Open P&L", Fmt.signedMoney(account.openPl), tint: Theme.gain(account.openPl))
                    Spacer()
                    mini("Positions", "\(positionCount)")
                }
            }
        }
    }

    private func mini(_ label: String, _ value: String, tint: Color = Theme.text) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label).font(.caption).foregroundStyle(Theme.textSecondary)
            Text(value).font(.subheadline.weight(.semibold).monospacedDigit()).foregroundStyle(tint)
        }
    }
}

struct PositionRow: View {
    let position: Snapshot.Position

    var body: some View {
        Card(padding: 14) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(position.symbol).font(.headline).foregroundStyle(Theme.text)
                        if position.isShort {
                            Badge(text: "SHORT", color: Theme.down)
                        }
                    }
                    if let industry = position.industry {
                        Text(industry).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
                    }
                    stop
                }
                Spacer()
                VStack(alignment: .trailing, spacing: 4) {
                    Text(Fmt.money(abs(position.marketValue))).font(.headline.monospacedDigit()).foregroundStyle(Theme.text)
                    HStack(spacing: 4) {
                        Text(Fmt.signedMoney(position.unrealizedPl))
                        if let pct = position.unrealizedPlPct {
                            Text("(\(Fmt.signedPct(pct)))")
                        }
                    }
                    .font(.footnote.weight(.medium).monospacedDigit())
                    .foregroundStyle(Theme.gain(position.unrealizedPl))
                }
            }
        }
    }

    @ViewBuilder private var stop: some View {
        if let stop = position.stopPrice {
            Text("Stop \(Fmt.money(stop, cents: true))").font(.caption).foregroundStyle(Theme.textMuted)
        } else {
            Label("No stop at the broker", systemImage: "exclamationmark.triangle.fill")
                .font(.caption).foregroundStyle(Theme.amber)
        }
    }
}

struct SignalRow: View {
    let signal: Snapshot.Signal

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(signal.ticker).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    if signal.held {
                        Badge(text: "HELD", color: Theme.blue)
                    }
                    if signal.convergence {
                        Badge(text: "CONVERGENCE", color: Theme.gold)
                    }
                }
                if let name = signal.name {
                    Text(name).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 4) {
                Text(String(format: "%.2f", signal.score)).font(.subheadline.monospacedDigit()).foregroundStyle(Theme.text)
                FillBar(fraction: signal.score, color: signal.convergence ? Theme.gold : Theme.blue).frame(width: 56)
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }
}

struct TradeRow: View {
    let trade: Snapshot.ClosedTrade

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(trade.ticker).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    if trade.direction == "short" {
                        Badge(text: "SHORT", color: Theme.down)
                    }
                }
                Text("Closed \(Fmt.dayLabel(trade.exitDate, weekday: false))")
                    .font(.caption).foregroundStyle(Theme.textSecondary)
            }
            Spacer()
            if let pnl = trade.pnl {
                VStack(alignment: .trailing, spacing: 2) {
                    Text(Fmt.signedMoney(pnl)).font(.subheadline.weight(.semibold).monospacedDigit())
                    if let pct = trade.pnlPct {
                        Text(Fmt.signedPct(pct)).font(.caption.monospacedDigit())
                    }
                }
                .foregroundStyle(Theme.gain(pnl))
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
    }
}
