import Foundation
@testable import Pulse
import XCTest

final class SnapshotTests: XCTestCase {
    private func fixtureData() throws -> Data {
        let bundle = Bundle(for: SnapshotTests.self)
        let url = try XCTUnwrap(bundle.url(forResource: "snapshot_v1", withExtension: "json"), "fixture missing from the test bundle")
        return try Data(contentsOf: url)
    }

    func testDecodesTheFixtureTheRustSideWrites() throws {
        let s = try SnapshotReader.decode(fixtureData())
        XCTAssertEqual(s.version, 1)
        let account = try XCTUnwrap(s.account)
        XCTAssertEqual(account.equity, 100_300)
        XCTAssertEqual(account.dayChange, 500)
        XCTAssertEqual(s.positions.count, 2)
        let short = try XCTUnwrap(s.positions.first { $0.symbol == "TSLA" })
        XCTAssertTrue(short.isShort)
        XCTAssertEqual(short.stopPrice, 270)
        XCTAssertEqual(s.briefing?.stories.count, 2)
        XCTAssertEqual(s.briefing?.stories.first?.isHero, true)
        XCTAssertEqual(s.signals.first?.ticker, "AAPL")
        XCTAssertEqual(s.recentTrades.map(\.ticker), ["AMD", "NVDA"])
        XCTAssertFalse(s.calendar.earnings.isEmpty)
        XCTAssertFalse(s.calendar.macroEvents.isEmpty)
        let risk = try XCTUnwrap(s.risk)
        XCTAssertEqual(risk.positionCount, 2)
        XCTAssertNotNil(risk.betaExposurePct)
        XCTAssertGreaterThan(risk.drawdown.points.count, 1)
    }

    /// Every key in the fixture is modelled: re-encoding the decoded value gives the same
    /// keys. A field added on the Rust side and forgotten here shows up as a difference.
    func testNoFieldInTheFixtureIsDroppedByTheModels() throws {
        let data = try fixtureData()
        let snapshot = try SnapshotReader.decode(data)
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let reencoded = try JSONSerialization.jsonObject(with: encoder.encode(snapshot))
        let original = try JSONSerialization.jsonObject(with: data)

        func keys(_ value: Any, _ path: String = "$") -> Set<String> {
            switch value {
            case let dict as [String: Any]:
                dict.reduce(into: Set<String>()) { acc, kv in
                    if kv.value is NSNull {
                        return
                    } // a null the phone models as an absent optional
                    acc.insert("\(path).\(kv.key)")
                    acc.formUnion(keys(kv.value, "\(path).\(kv.key)"))
                }
            case let array as [Any]:
                array.reduce(into: Set<String>()) { $0.formUnion(keys($1, path + "[]")) }
            default:
                []
            }
        }
        // risk.positions is always empty in the snapshot: the positions are listed at the top level.
        let ignored: Set = ["$.risk.positions"]
        XCTAssertEqual(keys(original).subtracting(ignored), keys(reencoded))
    }

    func testPositionsAndClosedTradesCarryTheirPlainLanguageExplanation() throws {
        let s = try SnapshotReader.decode(fixtureData())
        let held = try XCTUnwrap(s.positions.first?.explanation)
        XCTAssertFalse(held.why.isEmpty)
        XCTAssertNotNil(held.plan, "an open position says when it will sell")
        let closed = try XCTUnwrap(s.recentTrades.first?.explanation)
        XCTAssertNil(closed.plan, "a closed trade has no exit plan")
        XCTAssertNotNil(closed.now)
    }

    func testAnOlderFileWithoutExplanationsStillReads() throws {
        var obj = try XCTUnwrap(JSONSerialization.jsonObject(with: fixtureData()) as? [String: Any])
        var positions = try XCTUnwrap(obj["positions"] as? [[String: Any]])
        for i in positions.indices {
            positions[i].removeValue(forKey: "explanation")
        }
        obj["positions"] = positions
        let s = try SnapshotReader.decode(JSONSerialization.data(withJSONObject: obj))
        XCTAssertTrue(s.positions.allSatisfy { $0.explanation == nil })
    }

    func testANewerFormatAsksForAnUpdate() {
        let data = Data(#"{"version": 2, "future": true}"#.utf8)
        XCTAssertThrowsError(try SnapshotReader.decode(data)) { error in
            XCTAssertEqual(error as? SnapshotError, .needsUpdate(found: 2))
        }
    }

    func testGarbageIsAnUnreadableError() {
        XCTAssertThrowsError(try SnapshotReader.decode(Data("not json".utf8))) { error in
            guard case .unreadable = error as? SnapshotError else { return XCTFail("\(error)") }
        }
    }

    func testReadsTheFileFromTheFolderOrFromAPulseFolderInsideIt() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("pulse-reader-\(UUID().uuidString)")
        let nested = root.appendingPathComponent("Pulse")
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let data = try fixtureData()

        XCTAssertThrowsError(try SnapshotReader.readFile(inFolder: root)) { XCTAssertEqual($0 as? SnapshotError, .fileMissing) }
        try data.write(to: nested.appendingPathComponent(SnapshotReader.fileName))
        XCTAssertEqual(try SnapshotReader.readFile(inFolder: root), data, "iCloud Drive picked instead of Pulse")
        XCTAssertEqual(try SnapshotReader.readFile(inFolder: nested), data)
    }

    func testFormatting() {
        XCTAssertEqual(Fmt.money(1234.5), "$1,235")
        XCTAssertEqual(Fmt.signedMoney(-300), "−$300")
        XCTAssertEqual(Fmt.signedMoney(12.5, cents: true), "+$12.50")
        XCTAssertEqual(Fmt.signedPct(2.44), "+2.4%")
        XCTAssertEqual(Fmt.signedPct(-0.04, digits: 2), "−0.04%")
        XCTAssertEqual(Fmt.compactNumber(94_000_000_000), "94.0B")
        XCTAssertEqual(Fmt.relativeDays(0), "today")
        XCTAssertEqual(Fmt.relativeDays(-3), "3 days ago")
        XCTAssertEqual(Fmt.dayLabel("2026-10-14"), "Wed, Oct 14")
        XCTAssertNotNil(Fmt.timestamp("2026-10-11T12:00:00Z"))
    }

    @MainActor
    func testTheStoreKeepsTheLastGoodCopyWhenARefreshFails() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("pulse-store-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "pulse-tests-\(UUID().uuidString)"))
        let data = try fixtureData()
        let file = dir.appendingPathComponent(SnapshotReader.fileName)
        try data.write(to: file)

        let store = SnapshotStore(cacheURL: dir.appendingPathComponent("cache.json"), defaults: defaults, fixtureMode: false)
        await store.start()
        XCTAssertEqual(store.phase, .needsFolder, "no folder chosen yet")

        await store.chooseFolder(dir)
        XCTAssertEqual(store.phase, .ready)
        XCTAssertEqual(store.snapshot?.version, 1)
        XCTAssertNil(store.error)

        try Data("broken".utf8).write(to: file)
        await store.refresh()
        XCTAssertNotNil(store.error)
        XCTAssertEqual(store.snapshot?.version, 1, "the old copy stays on screen")

        // A new launch shows the cached copy before it reads anything.
        let relaunched = try SnapshotStore(cacheURL: dir.appendingPathComponent("cache.json"), defaults: XCTUnwrap(UserDefaults(suiteName: "pulse-tests-\(UUID().uuidString)")), fixtureMode: false)
        await relaunched.start()
        XCTAssertEqual(relaunched.snapshot?.version, 1)

        store.forgetFolder()
        XCTAssertNil(store.snapshot)
        XCTAssertEqual(store.phase, .needsFolder)
    }

    func testDaysAreCountedFromTodayOnThePhoneNotFromWhenTheMacWroteTheFile() throws {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = .current
        let noon = try XCTUnwrap(cal.date(from: DateComponents(year: 2026, month: 10, day: 12, hour: 12)))
        XCTAssertEqual(Fmt.daysFromToday("2026-10-12", now: noon, calendar: cal), 0)
        XCTAssertEqual(Fmt.daysFromToday("2026-10-13", now: noon, calendar: cal), 1)
        XCTAssertEqual(Fmt.daysFromToday("2026-10-10", now: noon, calendar: cal), -2)
        XCTAssertNil(Fmt.daysFromToday("soon", now: noon, calendar: cal))
    }

    func testAnOlderCopyDeliveredLateDoesNotReplaceANewerOne() throws {
        let base = try SnapshotReader.decode(fixtureData())
        let json = try XCTUnwrap(String(data: fixtureData(), encoding: .utf8))
        let older = try SnapshotReader.decode(Data(json.replacingOccurrences(of: base.generatedAt, with: "2020-01-01T00:00:00Z").utf8))
        XCTAssertTrue(SnapshotStore.isNewer(base, than: older))
        XCTAssertFalse(SnapshotStore.isNewer(older, than: base))
        XCTAssertTrue(SnapshotStore.isNewer(base, than: nil))
        XCTAssertTrue(SnapshotStore.isNewer(base, than: base), "the same copy again is harmless")
    }

    func testAccountAsOfIsReadWhenTheMacKeptAnOlderAccount() throws {
        let json = try XCTUnwrap(String(data: fixtureData(), encoding: .utf8))
        XCTAssertNil(try SnapshotReader.decode(fixtureData()).accountAsOf)
        let kept = json.replacingOccurrences(of: "\"account_as_of\": null", with: "\"account_as_of\": \"2026-10-11T12:00:00Z\"")
        XCTAssertNotEqual(kept, json, "the fixture carries the key")
        XCTAssertEqual(try SnapshotReader.decode(Data(kept.utf8)).accountAsOf, "2026-10-11T12:00:00Z")
    }
}
