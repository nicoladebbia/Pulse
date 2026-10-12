import Foundation
import Observation

@MainActor
@Observable
final class SnapshotStore {
    enum Phase: Equatable {
        case loading
        case needsFolder
        case ready
    }

    private(set) var phase: Phase = .loading
    private(set) var snapshot: Snapshot?
    /// Why the last refresh failed; the old snapshot stays on screen.
    private(set) var error: String?
    private(set) var isRefreshing = false
    /// When the phone last read the file successfully.
    private(set) var lastRead: Date?

    private let cacheURL: URL
    private let defaults: UserDefaults
    private let fixtureMode: Bool

    init(
        cacheURL: URL = SnapshotStore.defaultCacheURL(),
        defaults: UserDefaults = .standard,
        fixtureMode: Bool = SnapshotStore.launchedWithFixture
    ) {
        self.cacheURL = cacheURL
        self.defaults = defaults
        self.fixtureMode = fixtureMode
    }

    static var launchedWithFixture: Bool {
        #if DEBUG
            ProcessInfo.processInfo.arguments.contains("-PulseFixture")
        #else
            false
        #endif
    }

    static func defaultCacheURL() -> URL {
        let base = (try? FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true))
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("last-snapshot.json")
    }

    /// Show the last good copy at once, then look for a newer one.
    func start() async {
        if fixtureMode {
            loadFixture()
            return
        }
        if snapshot == nil, let data = try? Data(contentsOf: cacheURL), let cached = try? SnapshotReader.decode(data) {
            snapshot = cached
            phase = .ready
        }
        await refresh()
    }

    func refresh() async {
        guard !fixtureMode, !isRefreshing else { return }
        guard let folder = FolderBookmark.load(defaults: defaults) else {
            if snapshot == nil {
                phase = .needsFolder
            }
            return
        }
        isRefreshing = true
        defer { isRefreshing = false }
        do {
            let data = try await Task.detached(priority: .userInitiated) { try SnapshotReader.readFile(inFolder: folder) }.value
            let fresh = try SnapshotReader.decode(data)
            snapshot = fresh
            lastRead = .now
            error = nil
            phase = .ready
            try? data.write(to: cacheURL, options: .atomic)
        } catch {
            self.error = error.localizedDescription
            if snapshot == nil {
                phase = .ready
            }
        }
    }

    func chooseFolder(_ url: URL) async {
        do {
            try FolderBookmark.save(url, defaults: defaults)
            error = nil
            await refresh()
        } catch {
            self.error = "Could not remember that folder: \(error.localizedDescription)"
        }
    }

    func forgetFolder() {
        FolderBookmark.clear(defaults: defaults)
        try? FileManager.default.removeItem(at: cacheURL)
        snapshot = nil
        error = nil
        lastRead = nil
        phase = .needsFolder
    }

    /// Debug only: `-PulseFixture` shows the bundled sample, `-PulseFixture -PulseSnapshotFile <path>` a real file.
    private func loadFixture() {
        var override: URL?
        #if DEBUG
            let args = ProcessInfo.processInfo.arguments
            if let i = args.firstIndex(of: "-PulseSnapshotFile"), i + 1 < args.count {
                override = URL(fileURLWithPath: args[i + 1])
            }
        #endif
        guard let url = override ?? Bundle.main.url(forResource: "snapshot_v1", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let fixture = try? SnapshotReader.decode(data)
        else {
            error = "The bundled fixture is missing."
            phase = .ready
            return
        }
        snapshot = fixture
        lastRead = .now
        phase = .ready
    }
}
