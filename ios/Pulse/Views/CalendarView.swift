import SwiftUI

struct CalendarView: View {
    @Environment(SnapshotStore.self) private var store

    private struct Day: Identifiable {
        let date: String
        let daysUntil: Int
        var macro: [Snapshot.MacroEvent]
        var earnings: [Snapshot.Earnings]
        var id: String {
            date
        }
    }

    private func days(_ c: Snapshot.CalendarData) -> [Day] {
        var byDate: [String: Day] = [:]
        for e in c.macroEvents {
            byDate[e.date, default: Day(date: e.date, daysUntil: e.daysAway, macro: [], earnings: [])].macro.append(e)
        }
        for e in c.earnings {
            byDate[e.date, default: Day(date: e.date, daysUntil: e.daysAway, macro: [], earnings: [])].earnings.append(e)
        }
        return byDate.values.sorted { $0.date < $1.date }
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    SnapshotHeader()
                    if let calendar = store.snapshot?.calendar {
                        if !calendar.heldInBlackout.isEmpty {
                            NoticeCard(
                                title: "You hold stocks reporting soon",
                                message: "\(calendar.heldInBlackout.joined(separator: ", ")) report within 3 trading days. Pulse will not add to them."
                            )
                        }
                        if !calendar.hasEarningsData {
                            NoticeCard(
                                title: "Earnings dates not loaded",
                                message: "The Mac has not fetched them yet (it needs a Finnhub key). Macro dates still show.",
                                color: Theme.textSecondary
                            )
                        }
                        let all = days(calendar)
                        if all.isEmpty {
                            NoticeCard(title: "Nothing coming up", message: "No earnings for your stocks or macro releases in the next weeks.", color: Theme.textSecondary)
                        }
                        ForEach(all) { DayBlock(day: $0.date, daysUntil: $0.daysUntil, macro: $0.macro, earnings: $0.earnings) }
                        Text("Earnings shown: stocks you hold and strong signals, next two weeks.")
                            .font(.caption).foregroundStyle(Theme.textMuted).padding(.horizontal, 4)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .refreshable { await store.refresh() }
            .navigationTitle("Calendar")
            .settingsToolbar()
            .pulseScreen()
        }
    }
}

private struct DayBlock: View {
    let day: String
    let daysUntil: Int
    let macro: [Snapshot.MacroEvent]
    let earnings: [Snapshot.Earnings]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                Text(Fmt.dayLabel(day))
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(daysUntil == 0 ? Theme.amber : (daysUntil < 0 ? Theme.textMuted : Theme.text))
                Text(Fmt.relativeDays(daysUntil)).font(.caption).foregroundStyle(Theme.textMuted)
            }
            .padding(.horizontal, 4)
            .padding(.top, 6)
            Card(padding: 0) {
                VStack(spacing: 0) {
                    let rows = macro.count + earnings.count
                    ForEach(Array(macro.enumerated()), id: \.element.id) { i, m in
                        MacroRow(event: m)
                        if i < rows - 1 {
                            Divider().overlay(Theme.border)
                        }
                    }
                    ForEach(Array(earnings.enumerated()), id: \.element.id) { i, e in
                        EarningsRow(item: e)
                        if macro.count + i < rows - 1 {
                            Divider().overlay(Theme.border)
                        }
                    }
                }
            }
            .opacity(daysUntil < 0 ? 0.6 : 1)
        }
    }
}

private struct MacroRow: View {
    let event: Snapshot.MacroEvent

    var body: some View {
        HStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 2).fill(Theme.blue).frame(width: 3, height: 34)
            VStack(alignment: .leading, spacing: 2) {
                Text(event.title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text([event.timeEt + " ET", event.note].compactMap(\.self).joined(separator: " · "))
                    .font(.caption).foregroundStyle(Theme.textSecondary)
            }
            Spacer()
            Badge(text: event.kind.uppercased(), color: Theme.blue)
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
    }
}

private struct EarningsRow: View {
    let item: Snapshot.Earnings

    private var hour: String {
        switch item.hour {
        case "bmo": "Before the open"
        case "amc": "After the close"
        case "dmh": "During the day"
        default: "Time not announced"
        }
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(item.ticker).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                    if item.held {
                        Badge(text: "HELD", color: Theme.blue)
                    }
                    if let s = item.signalScore, s >= 0.2 {
                        Badge(text: "SIGNAL", color: Theme.gold)
                    }
                }
                if let name = item.name {
                    Text(name).font(.caption).foregroundStyle(Theme.textSecondary).lineLimit(1)
                }
                if item.blackout, item.daysAway >= 0 {
                    Text("No new buys").font(.caption2.weight(.semibold)).foregroundStyle(Theme.amber)
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 3) {
                Text(hour).font(.caption).foregroundStyle(Theme.textSecondary)
                if let eps = item.epsEstimate {
                    Text("EPS est. \(String(format: "%.2f", eps))").font(.caption2.monospacedDigit()).foregroundStyle(Theme.textMuted)
                }
                if let revenue = item.revenueEstimate {
                    Text("Rev. est. \(Fmt.compactNumber(revenue))").font(.caption2.monospacedDigit()).foregroundStyle(Theme.textMuted)
                }
            }
        }
        .padding(.horizontal, 14).padding(.vertical, 10)
    }
}
