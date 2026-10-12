import SwiftUI

struct RootView: View {
    @Environment(SnapshotStore.self) private var store
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        Group {
            switch store.phase {
            case .loading:
                ProgressView().tint(Theme.textSecondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            case .needsFolder:
                SetupView()
            case .ready:
                MainTabs()
            }
        }
        .background(Theme.bg.ignoresSafeArea())
        .task { await store.start() }
        .onChange(of: scenePhase) { _, new in
            if new == .active {
                Task { await store.refresh() }
            }
        }
    }
}

struct MainTabs: View {
    enum Tab: String { case briefing, portfolio, risk, calendar }

    @State private var tab: Tab = Self.initialTab

    private static var initialTab: Tab {
        #if DEBUG
            let args = ProcessInfo.processInfo.arguments
            if let i = args.firstIndex(of: "-PulseTab"), i + 1 < args.count, let t = Tab(rawValue: args[i + 1]) {
                return t
            }
        #endif
        return .briefing
    }

    var body: some View {
        TabView(selection: $tab) {
            BriefingView()
                .tabItem { Label("Briefing", systemImage: "newspaper") }
                .tag(Tab.briefing)
            PortfolioView()
                .tabItem { Label("Portfolio", systemImage: "chart.line.uptrend.xyaxis") }
                .tag(Tab.portfolio)
            RiskView()
                .tabItem { Label("Risk", systemImage: "shield.lefthalf.filled") }
                .tag(Tab.risk)
            CalendarView()
                .tabItem { Label("Calendar", systemImage: "calendar") }
                .tag(Tab.calendar)
        }
        .toolbarBackground(Theme.card, for: .tabBar)
        .toolbarBackground(.visible, for: .tabBar)
        .toolbarColorScheme(.dark, for: .tabBar)
    }
}
