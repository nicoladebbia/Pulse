import Charts
import SwiftUI

struct RiskView: View {
    @Environment(SnapshotStore.self) private var store

    var body: some View {
        NavigationStack {
            ScrollViewReader { proxy in
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        SnapshotHeader()
                        if let risk = store.snapshot?.risk {
                            if let asOf = store.snapshot?.accountAsOf {
                                NoticeCard(
                                    title: "Risk figures are older",
                                    message: "The Mac could not reach Alpaca for the latest update. Exposure, stops and stress tests are from \(Fmt.timestamp(asOf).map { $0.formatted(date: .abbreviated, time: .shortened) } ?? asOf)."
                                )
                            }
                            if !risk.warnings.isEmpty {
                                NoticeCard(title: "Heads up", message: risk.warnings.joined(separator: "\n"))
                            }
                            exposure(risk)
                            stops(risk)
                            if !risk.stress.isEmpty {
                                stress(risk).id("stress")
                            }
                            if !risk.industries.isEmpty {
                                industries(risk).id("industries")
                            }
                            if risk.drawdown.points.count > 1 {
                                drawdown(risk.drawdown).id("drawdown")
                            }
                        } else {
                            NoticeCard(
                                title: "No risk numbers in this update",
                                message: "Risk needs the account, which the Mac could not read from Alpaca when it wrote this copy."
                            )
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 24)
                    .modifier(DebugScroll(proxy: proxy))
                }
            }
            .refreshable { await store.refresh() }
            .navigationTitle("Risk")
            .settingsToolbar()
            .pulseScreen()
        }
    }

    private func exposure(_ r: Snapshot.Risk) -> some View {
        let columns = [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)]
        return LazyVGrid(columns: columns, spacing: 10) {
            StatTile(label: "Invested (gross)", value: Fmt.pct(r.grossPct, digits: 0), detail: "\(Fmt.money(r.longValue + abs(r.shortValue))) of \(Fmt.money(r.equity))")
            StatTile(label: "In cash", value: Fmt.pct(r.cashPct, digits: 0), detail: Fmt.money(r.cash))
            StatTile(
                label: "Market exposure",
                value: r.betaExposurePct.map { Fmt.pct($0, digits: 0) } ?? "—",
                detail: r.betaExposurePct == nil ? "Price history missing" : "Beta-weighted, vs the S&P"
            )
            StatTile(
                label: "Biggest position",
                value: Fmt.pct(r.largestPositionPct, digits: 1),
                detail: "\(r.positionCount) positions",
                tint: r.largestPositionPct > 10 ? Theme.amber : Theme.text
            )
        }
    }

    private func stops(_ r: Snapshot.Risk) -> some View {
        Card {
            VStack(alignment: .leading, spacing: 8) {
                Text("If every stop fills").font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                HStack(alignment: .firstTextBaseline) {
                    Text(Fmt.signedMoney(-abs(r.stopLossDollars)))
                        .font(.title.weight(.bold).monospacedDigit()).foregroundStyle(Theme.down)
                    Text("(\(Fmt.pct(abs(r.stopLossPct), digits: 1)) of the account)")
                        .font(.subheadline).foregroundStyle(Theme.textSecondary)
                }
            }
        }
    }

    private func stress(_ r: Snapshot.Risk) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionTitle(text: "Stress test")
            Card(padding: 0) {
                VStack(spacing: 0) {
                    ForEach(Array(r.stress.enumerated()), id: \.element.id) { i, s in
                        HStack {
                            Text(s.label).font(.subheadline).foregroundStyle(Theme.text)
                            Spacer()
                            VStack(alignment: .trailing, spacing: 2) {
                                Text(Fmt.signedMoney(s.pnlDollars)).font(.subheadline.weight(.semibold).monospacedDigit())
                                Text(Fmt.signedPct(s.pnlPct, digits: 2)).font(.caption.monospacedDigit())
                            }
                            .foregroundStyle(Theme.gain(s.pnlDollars))
                        }
                        .padding(.horizontal, 14).padding(.vertical, 10)
                        if i < r.stress.count - 1 {
                            Divider().overlay(Theme.border)
                        }
                    }
                }
            }
        }
    }

    private func industries(_ r: Snapshot.Risk) -> some View {
        let top = r.industries.sorted { $0.grossPct > $1.grossPct }.prefix(8)
        let scale = max(top.map(\.grossPct).max() ?? 1, 1)
        return VStack(alignment: .leading, spacing: 8) {
            SectionTitle(text: "By industry", trailing: "limit 30%")
            Card {
                VStack(spacing: 12) {
                    ForEach(Array(top)) { i in
                        VStack(spacing: 5) {
                            HStack {
                                Text(i.industry).font(.subheadline).foregroundStyle(Theme.text).lineLimit(1)
                                Spacer()
                                Text(Fmt.pct(i.grossPct, digits: 0)).font(.subheadline.monospacedDigit())
                                    .foregroundStyle(i.grossPct > 30 ? Theme.amber : Theme.textSecondary)
                            }
                            FillBar(fraction: i.grossPct / scale, color: i.grossPct > 30 ? Theme.amber : Theme.blue)
                        }
                    }
                }
            }
        }
    }

    private func drawdown(_ d: Snapshot.Drawdown) -> some View {
        let points = d.points.compactMap { p in Fmt.day(p.date).map { ($0, p.drawdownPct) } }
        return VStack(alignment: .leading, spacing: 8) {
            SectionTitle(text: "Drawdown from the peak")
            Card {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        StatTileMini(label: "Now", value: Fmt.pct(d.currentPct, digits: 1))
                        Spacer()
                        StatTileMini(label: "Worst", value: Fmt.pct(d.maxPct, digits: 1))
                        Spacer()
                        StatTileMini(label: "Peak", value: Fmt.money(d.peakEquity))
                    }
                    Chart(points, id: \.0) { p in
                        AreaMark(x: .value("Day", p.0), yStart: .value("Zero", 0), yEnd: .value("Drawdown", p.1))
                            .foregroundStyle(Theme.down.opacity(0.18))
                        LineMark(x: .value("Day", p.0), y: .value("Drawdown", p.1))
                            .foregroundStyle(Theme.down)
                            .interpolationMethod(.monotone)
                    }
                    .chartYAxis {
                        AxisMarks(position: .leading) { v in
                            AxisGridLine().foregroundStyle(Theme.border)
                            AxisValueLabel {
                                if let d = v.as(Double.self) {
                                    Text("\(Int(d))%").foregroundStyle(Theme.textMuted)
                                }
                            }
                        }
                    }
                    .chartXAxis {
                        AxisMarks(values: .automatic(desiredCount: 4)) { _ in
                            AxisValueLabel(format: .dateTime.month(.abbreviated).day()).foregroundStyle(Theme.textMuted)
                        }
                    }
                    .frame(height: 150)
                }
            }
        }
    }
}

struct StatTileMini: View {
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label).font(.caption).foregroundStyle(Theme.textSecondary)
            Text(value).font(.subheadline.weight(.semibold).monospacedDigit()).foregroundStyle(Theme.text)
        }
    }
}
