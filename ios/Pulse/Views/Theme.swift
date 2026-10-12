import SwiftUI

/// Same palette as the Mac app (`src/app.css`), dark only.
enum Theme {
    static let bg = Color(hex: 0x0A0A0F)
    static let card = Color(hex: 0x12121A)
    static let cardRaised = Color(hex: 0x1A1A25)
    static let border = Color(hex: 0x1E1E2E)
    static let text = Color(hex: 0xE4E4EF)
    static let textSecondary = Color(hex: 0x8888A0)
    static let textMuted = Color(hex: 0x55556A)

    static let up = Color(hex: 0x22C55E)
    static let down = Color(hex: 0xEF4444)
    static let amber = Color(hex: 0xF59E0B)
    static let blue = Color(hex: 0x3B82F6)
    static let gold = Color(hex: 0xC9A84C)

    static func sector(_ name: String) -> Color {
        switch name.lowercased() {
        case "ai": blue
        case "miami": Color(hex: 0xF97316)
        case "italy": up
        case "tech": Color(hex: 0xA855F7)
        case "wealth", "finance", "financial": gold
        default: textSecondary
        }
    }

    static func gain(_ v: Double) -> Color {
        v > 0 ? up : (v < 0 ? down : textSecondary)
    }
}

extension Color {
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}
