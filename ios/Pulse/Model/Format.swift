import Foundation

/// Number and date text for the screens. Money is whole dollars unless small.
enum Fmt {
    private static let usd: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .currency
        f.currencyCode = "USD"
        f.maximumFractionDigits = 0
        f.roundingMode = .halfUp
        return f
    }()

    private static let usdCents: NumberFormatter = {
        let f = NumberFormatter()
        f.numberStyle = .currency
        f.currencyCode = "USD"
        f.maximumFractionDigits = 2
        f.roundingMode = .halfUp
        return f
    }()

    static func money(_ v: Double, cents: Bool = false) -> String {
        (cents ? usdCents : usd).string(from: NSNumber(value: v)) ?? "—"
    }

    /// "+$1,200" / "−$300"
    static func signedMoney(_ v: Double, cents: Bool = false) -> String {
        let body = money(abs(v), cents: cents)
        if rounded(v, digits: cents ? 2 : 0) == 0 {
            return body
        }
        return (v > 0 ? "+" : "−") + body
    }

    /// `v` at `digits` decimals, so a value that displays as zero gets no sign.
    private static func rounded(_ v: Double, digits: Int) -> Double {
        let f = pow(10.0, Double(digits))
        return (v * f).rounded() / f
    }

    static func pct(_ v: Double, digits: Int = 1) -> String {
        String(format: "%.\(digits)f%%", v)
    }

    /// "+2.4%" / "−0.8%"
    static func signedPct(_ v: Double, digits: Int = 1) -> String {
        let body = String(format: "%.\(digits)f%%", abs(v))
        if rounded(v, digits: digits) == 0 {
            return body
        }
        return (v > 0 ? "+" : "−") + body
    }

    static func compactNumber(_ v: Double) -> String {
        let a = abs(v)
        switch a {
        case 1e12...: return String(format: "%.1fT", v / 1e12)
        case 1e9...: return String(format: "%.1fB", v / 1e9)
        case 1e6...: return String(format: "%.1fM", v / 1e6)
        case 1e3...: return String(format: "%.1fK", v / 1e3)
        default: return String(format: "%.0f", v)
        }
    }

    // MARK: Dates

    private static let dayParser: DateFormatter = {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    /// `YYYY-MM-DD` as a local date, so it never shifts a day across time zones.
    static func day(_ s: String) -> Date? {
        dayParser.date(from: String(s.prefix(10)))
    }

    static func timestamp(_ s: String) -> Date? {
        try? Date(s, strategy: .iso8601)
    }

    static func dayLabel(_ s: String, weekday: Bool = true) -> String {
        guard let d = day(s) else { return s }
        if weekday {
            return d.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day())
        }
        return d.formatted(.dateTime.month(.abbreviated).day())
    }

    static func relativeDays(_ n: Int) -> String {
        switch n {
        case 0: "today"
        case 1: "tomorrow"
        case -1: "yesterday"
        case 2...: "in \(n) days"
        default: "\(-n) days ago"
        }
    }

    static func ago(_ date: Date, now: Date = .now) -> String {
        let s = now.timeIntervalSince(date)
        if s < 90 {
            return "just now"
        }
        if s < 3600 {
            return "\(Int(s / 60)) min ago"
        }
        if s < 86400 {
            return "\(Int(s / 3600)) h ago"
        }
        return "\(Int(s / 86400)) d ago"
    }
}
