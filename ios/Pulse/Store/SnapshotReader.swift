import Foundation

enum SnapshotError: LocalizedError, Equatable {
    case noFolder
    case folderUnavailable
    case fileMissing
    case needsUpdate(found: Int)
    case unreadable(String)

    var errorDescription: String? {
        switch self {
        case .noFolder: "Choose the Pulse folder in iCloud Drive first."
        case .folderUnavailable: "The Pulse folder can no longer be opened. Choose it again."
        case .fileMissing: "pulse-snapshot.json is not in that folder yet. Pulse on the Mac writes it after its next run."
        case let .needsUpdate(found): "The Mac is sending a newer format (v\(found)). Update this app."
        case let .unreadable(why): "The snapshot could not be read: \(why)"
        }
    }
}

enum SnapshotReader {
    static let fileName = "pulse-snapshot.json"
    /// Highest `version` this build understands.
    static let supportedVersion = 1

    static func decode(_ data: Data) throws -> Snapshot {
        struct Header: Decodable { let version: Int }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        do {
            let header = try decoder.decode(Header.self, from: data)
            if header.version > supportedVersion {
                throw SnapshotError.needsUpdate(found: header.version)
            }
            return try decoder.decode(Snapshot.self, from: data)
        } catch let e as SnapshotError {
            throw e
        } catch {
            throw SnapshotError.unreadable(String(describing: error))
        }
    }

    /// Read the file from the folder the user picked. The picked folder is either
    /// `Pulse` itself or iCloud Drive with a `Pulse` folder inside. A file that is
    /// only in iCloud is downloaded first; the coordinated read waits for it.
    /// Blocking: call off the main thread.
    static func readFile(inFolder folder: URL) throws -> Data {
        let scoped = folder.startAccessingSecurityScopedResource()
        defer {
            if scoped {
                folder.stopAccessingSecurityScopedResource()
            }
        }

        let candidates = [
            folder.appendingPathComponent(fileName),
            folder.appendingPathComponent("Pulse", isDirectory: true).appendingPathComponent(fileName),
        ]
        var lastError: Error?
        for file in candidates {
            try? FileManager.default.startDownloadingUbiquitousItem(at: file)
            var coordinationError: NSError?
            var result: Result<Data, Error>?
            NSFileCoordinator(filePresenter: nil).coordinate(readingItemAt: file, options: [], error: &coordinationError) { url in
                result = Result { try Data(contentsOf: url) }
            }
            if let data = try? result?.get() {
                return data
            }
            lastError = coordinationError ?? {
                if case let .failure(e)? = result {
                    e
                } else {
                    nil
                }
            }()
        }
        if let e = lastError as NSError?, e.domain == NSCocoaErrorDomain,
           e.code == NSFileReadNoSuchFileError || e.code == NSFileNoSuchFileError
        {
            throw SnapshotError.fileMissing
        }
        if let lastError {
            throw SnapshotError.unreadable(lastError.localizedDescription)
        }
        throw SnapshotError.fileMissing
    }
}

/// Remembers the folder across launches. A security-scoped bookmark is how an
/// iOS app keeps access to a folder the user picked in Files.
enum FolderBookmark {
    private static let key = "pulse.folderBookmark"

    static func save(_ url: URL, defaults: UserDefaults = .standard) throws {
        let scoped = url.startAccessingSecurityScopedResource()
        defer {
            if scoped {
                url.stopAccessingSecurityScopedResource()
            }
        }
        try defaults.set(url.bookmarkData(), forKey: key)
    }

    static func load(defaults: UserDefaults = .standard) -> URL? {
        guard let data = defaults.data(forKey: key) else { return nil }
        var stale = false
        guard let url = try? URL(resolvingBookmarkData: data, bookmarkDataIsStale: &stale) else { return nil }
        if stale {
            try? save(url, defaults: defaults)
        }
        return url
    }

    static func clear(defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key)
    }
}
