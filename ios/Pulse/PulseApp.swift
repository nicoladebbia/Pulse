import SwiftUI

@main
struct PulseApp: App {
    @State private var store = SnapshotStore()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(store)
                .preferredColorScheme(.dark)
                .tint(Theme.blue)
        }
    }
}
