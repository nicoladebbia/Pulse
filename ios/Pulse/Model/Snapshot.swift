import Foundation

/// The file the Mac writes to iCloud Drive (`pulse-snapshot.json`). Field names are
/// the Rust side's snake_case, decoded with `.convertFromSnakeCase`. The contract
/// is `crates/pulse-snapshot/tests/fixtures/snapshot_v1.json`, which both sides test.
struct Snapshot: Codable, Sendable, Equatable {
    let version: Int
    /// RFC 3339, UTC.
    let generatedAt: String
    /// Nil when the Mac could not reach Alpaca for this update.
    let account: Account?
    /// When `account`, `positions` and `risk` were read from Alpaca, if that was an
    /// earlier run (the Mac could not reach Alpaca for this one). Nil when current.
    let accountAsOf: String?
    let positions: [Position]
    let risk: Risk?
    let briefing: Briefing?
    let signals: [Signal]
    let calendar: CalendarData
    let recentTrades: [ClosedTrade]

    struct Account: Codable, Sendable, Equatable {
        let equity: Double
        let cash: Double
        let dayChange: Double?
        let dayChangePct: Double?
        let openPl: Double
    }

    struct Position: Codable, Sendable, Equatable, Identifiable {
        let symbol: String
        /// "long" or "short"
        let side: String
        let qty: Double
        let price: Double
        let marketValue: Double
        let unrealizedPl: Double
        let unrealizedPlPct: Double?
        let stopPrice: Double?
        let industry: String?
        var id: String {
            symbol
        }

        var isShort: Bool {
            side == "short"
        }
    }

    struct Briefing: Codable, Sendable, Equatable {
        /// `YYYY-MM-DD`
        let date: String
        let timeLabel: String?
        let executiveSummary: String?
        let stories: [Story]
    }

    struct Story: Codable, Sendable, Equatable, Identifiable {
        let id: Int
        let sector: String
        let headline: String
        let summary: String
        let whyItMatters: String?
        let sourceName: String?
        let url: String?
        let isHero: Bool
    }

    struct Signal: Codable, Sendable, Equatable, Identifiable {
        let ticker: String
        let name: String?
        let score: Double
        let convergence: Bool
        let held: Bool
        var id: String {
            ticker
        }
    }

    struct ClosedTrade: Codable, Sendable, Equatable, Identifiable {
        let ticker: String
        let direction: String
        let entryDate: String
        let exitDate: String
        let pnl: Double?
        let pnlPct: Double?
        var id: String {
            "\(ticker)-\(exitDate)-\(entryDate)"
        }
    }

    struct CalendarData: Codable, Sendable, Equatable {
        let today: String
        let days: Int
        let earnings: [Earnings]
        let macroEvents: [MacroEvent]
        let hasEarningsData: Bool
        let earningsUpdatedAt: String?
        let heldInBlackout: [String]
    }

    struct Earnings: Codable, Sendable, Equatable, Identifiable {
        let ticker: String
        let name: String?
        let date: String
        /// "bmo" before the open, "amc" after the close, "dmh" during hours.
        let hour: String?
        let epsEstimate: Double?
        let revenueEstimate: Double?
        let daysUntil: Int
        let held: Bool
        /// Inside the window where the bot will not open a position.
        let blackout: Bool
        let signalScore: Double?
        var id: String {
            "\(ticker)-\(date)"
        }

        /// Days from today on this phone, not from when the Mac wrote the file.
        var daysAway: Int {
            Fmt.daysFromToday(date) ?? daysUntil
        }
    }

    struct MacroEvent: Codable, Sendable, Equatable, Identifiable {
        let date: String
        /// "fomc", "cpi", "ppi" or "jobs"
        let kind: String
        let title: String
        let timeEt: String
        let note: String?
        let daysUntil: Int
        var id: String {
            "\(date)-\(kind)"
        }

        var daysAway: Int {
            Fmt.daysFromToday(date) ?? daysUntil
        }
    }

    struct Risk: Codable, Sendable, Equatable {
        let equity: Double
        let cash: Double
        let longValue: Double
        let shortValue: Double
        let grossPct: Double
        let netPct: Double
        let cashPct: Double
        let positionCount: Int
        let largestPositionPct: Double
        let industries: [Industry]
        let stopLossDollars: Double
        let stopLossPct: Double
        let positionsWithoutStop: Int
        let betaExposurePct: Double?
        let betaCoveragePct: Double
        let stress: [Stress]
        let drawdown: Drawdown
        let warnings: [String]
    }

    struct Industry: Codable, Sendable, Equatable, Identifiable {
        let industry: String
        let count: Int
        let longValue: Double
        let shortValue: Double
        let netValue: Double
        let grossPct: Double
        var id: String {
            industry
        }
    }

    struct Stress: Codable, Sendable, Equatable, Identifiable {
        let label: String
        let spyMovePct: Double
        let pnlDollars: Double
        let pnlPct: Double
        var id: String {
            label
        }
    }

    struct Drawdown: Codable, Sendable, Equatable {
        let currentPct: Double
        let maxPct: Double
        let peakEquity: Double
        let peakDate: String?
        let points: [Point]
    }

    struct Point: Codable, Sendable, Equatable, Identifiable {
        let date: String
        let equity: Double
        let drawdownPct: Double
        var id: String {
            date
        }
    }
}
