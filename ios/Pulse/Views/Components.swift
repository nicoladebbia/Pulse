import SwiftUI

struct Card<Content: View>: View {
    var padding: CGFloat = 16
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.card, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(Theme.border, lineWidth: 1))
    }
}

struct SectionTitle: View {
    let text: String
    var trailing: String?

    var body: some View {
        HStack {
            Text(text.uppercased())
                .font(.caption.weight(.semibold))
                .tracking(1.1)
                .foregroundStyle(Theme.textSecondary)
            Spacer()
            if let trailing {
                Text(trailing).font(.caption).foregroundStyle(Theme.textMuted)
            }
        }
        .padding(.horizontal, 4)
        .padding(.top, 8)
    }
}

struct Badge: View {
    let text: String
    var color: Color = Theme.textSecondary

    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .bold))
            .tracking(0.5)
            .foregroundStyle(color)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(color.opacity(0.14), in: Capsule())
    }
}

struct StatTile: View {
    let label: String
    let value: String
    var detail: String?
    var tint: Color = Theme.text

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label).font(.caption).foregroundStyle(Theme.textSecondary)
            Text(value).font(.title3.weight(.semibold).monospacedDigit()).foregroundStyle(tint)
            if let detail {
                Text(detail).font(.caption2).foregroundStyle(Theme.textMuted)
            }
        }
        .frame(maxWidth: .infinity, minHeight: 84, alignment: .topLeading)
        .padding(14)
        .background(Theme.card, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).stroke(Theme.border, lineWidth: 1))
    }
}

/// A horizontal bar filled to `fraction` (0...1).
struct FillBar: View {
    let fraction: Double
    var color: Color = Theme.blue

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Theme.cardRaised)
                Capsule().fill(color).frame(width: max(0, min(1, fraction.isFinite ? fraction : 0)) * geo.size.width)
            }
        }
        .frame(height: 6)
    }
}

struct NoticeCard: View {
    let title: String
    let message: String
    var color: Color = Theme.amber

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(color)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
                Text(message).font(.footnote).foregroundStyle(Theme.textSecondary)
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(color.opacity(0.10), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).stroke(color.opacity(0.35), lineWidth: 1))
    }
}

/// "Updated 3 h ago", plus a notice when the copy is old or the last refresh failed.
struct SnapshotHeader: View {
    @Environment(SnapshotStore.self) private var store

    private var generated: Date? {
        store.snapshot.flatMap { Fmt.timestamp($0.generatedAt) }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                Circle().fill(isStale ? Theme.amber : Theme.up).frame(width: 6, height: 6)
                Text(generated.map { "Updated \(Fmt.ago($0))" } ?? "Waiting for the Mac")
                    .font(.footnote)
                    .foregroundStyle(Theme.textSecondary)
                if store.isRefreshing {
                    ProgressView().controlSize(.mini)
                }
            }
            if let error = store.error {
                NoticeCard(title: "Could not refresh", message: "\(error) Showing the last copy.", color: Theme.down)
            } else if isStale, let generated {
                NoticeCard(
                    title: "This copy is \(Fmt.ago(generated).replacingOccurrences(of: " ago", with: "")) old",
                    message: "Pulse on the Mac writes it after each run. Check that the Mac was on and online."
                )
            }
        }
    }

    private var isStale: Bool {
        guard let generated else { return false }
        return Date.now.timeIntervalSince(generated) > 36 * 3600
    }
}

/// The gear that opens Settings, on every tab.
struct SettingsToolbar: ViewModifier {
    @State private var showSettings = false

    func body(content: Content) -> some View {
        content
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showSettings = true } label: { Image(systemName: "gearshape") }
                        .accessibilityLabel("Settings")
                }
            }
            .sheet(isPresented: $showSettings) { SettingsView() }
    }
}

extension View {
    func settingsToolbar() -> some View {
        modifier(SettingsToolbar())
    }

    /// The screen background and a dark navigation bar.
    func pulseScreen() -> some View {
        background(Theme.bg.ignoresSafeArea())
            .toolbarBackground(Theme.bg, for: .navigationBar)
            .toolbarColorScheme(.dark, for: .navigationBar)
    }
}

/// Debug only: `-PulseScrollTo <id>` scrolls a tab to the section with that id, for screenshots.
struct DebugScroll: ViewModifier {
    let proxy: ScrollViewProxy

    func body(content: Content) -> some View {
        #if DEBUG
            content.task {
                let args = ProcessInfo.processInfo.arguments
                guard let i = args.firstIndex(of: "-PulseScrollTo"), i + 1 < args.count else { return }
                try? await Task.sleep(for: .milliseconds(600))
                proxy.scrollTo(args[i + 1], anchor: .top)
            }
        #else
            content
        #endif
    }
}
