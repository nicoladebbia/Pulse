import SwiftUI

struct BriefingView: View {
    @Environment(SnapshotStore.self) private var store
    @State private var summaryOpen = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    SnapshotHeader()
                    if let briefing = store.snapshot?.briefing {
                        header(briefing)
                        if let summary = briefing.executiveSummary {
                            Card {
                                VStack(alignment: .leading, spacing: 8) {
                                    Text("TODAY IN ONE PARAGRAPH")
                                        .font(.caption2.weight(.bold)).tracking(1.1).foregroundStyle(Theme.gold)
                                    Text(summary)
                                        .font(.body).foregroundStyle(Theme.text)
                                        .lineLimit(summaryOpen ? nil : 6)
                                        .fixedSize(horizontal: false, vertical: true)
                                    Text(summaryOpen ? "Show less" : "Read more")
                                        .font(.footnote.weight(.semibold)).foregroundStyle(Theme.blue)
                                }
                            }
                            .onTapGesture { withAnimation(.easeInOut(duration: 0.2)) { summaryOpen.toggle() } }
                            .accessibilityAddTraits(.isButton)
                        }
                        ForEach(briefing.stories) { story in
                            NavigationLink { StoryDetail(story: story) } label: { StoryRow(story: story) }
                                .buttonStyle(.plain)
                        }
                    } else {
                        NoticeCard(title: "No briefing yet", message: "The Mac has not finished a briefing. It appears here after the next one.")
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .refreshable { await store.refresh() }
            .navigationTitle("Briefing")
            .settingsToolbar()
            .pulseScreen()
        }
    }

    private func header(_ b: Snapshot.Briefing) -> some View {
        HStack {
            Text(Fmt.dayLabel(b.date)).font(.title3.weight(.semibold)).foregroundStyle(Theme.text)
            if let label = b.timeLabel {
                Text(label).font(.subheadline).foregroundStyle(Theme.textSecondary)
            }
            Spacer()
            Text("\(b.stories.count) stories").font(.footnote).foregroundStyle(Theme.textMuted)
        }
    }
}

struct StoryRow: View {
    let story: Snapshot.Story

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Circle().fill(Theme.sector(story.sector)).frame(width: 7, height: 7)
                    Text(story.sector.uppercased())
                        .font(.caption2.weight(.bold)).tracking(1).foregroundStyle(Theme.sector(story.sector))
                    if story.isHero {
                        Badge(text: "TOP STORY", color: Theme.gold)
                    }
                    Spacer()
                    if let source = story.sourceName {
                        Text(source).font(.caption).foregroundStyle(Theme.textMuted).lineLimit(1)
                    }
                }
                Text(story.headline)
                    .font(story.isHero ? .title3.weight(.semibold) : .headline)
                    .foregroundStyle(Theme.text)
                    .multilineTextAlignment(.leading)
                    .fixedSize(horizontal: false, vertical: true)
                Text(story.summary)
                    .font(.subheadline).foregroundStyle(Theme.textSecondary).lineLimit(3)
                    .multilineTextAlignment(.leading)
            }
        }
    }
}

struct StoryDetail: View {
    let story: Snapshot.Story

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 8) {
                    Circle().fill(Theme.sector(story.sector)).frame(width: 8, height: 8)
                    Text(story.sector.uppercased())
                        .font(.caption.weight(.bold)).tracking(1).foregroundStyle(Theme.sector(story.sector))
                    if let source = story.sourceName {
                        Text("· \(source)").font(.caption).foregroundStyle(Theme.textMuted)
                    }
                }
                Text(story.headline).font(.title2.weight(.bold)).foregroundStyle(Theme.text)
                Text(story.summary).font(.body).foregroundStyle(Theme.text).fixedSize(horizontal: false, vertical: true)
                if let why = story.whyItMatters {
                    Card {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("WHY IT MATTERS").font(.caption2.weight(.bold)).tracking(1.1).foregroundStyle(Theme.gold)
                            Text(why).font(.subheadline).foregroundStyle(Theme.text).fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
                if let link = story.url.flatMap(URL.init(string:)), link.scheme == "https" || link.scheme == "http" {
                    Link(destination: link) {
                        Label("Read at the source", systemImage: "arrow.up.right.square")
                            .font(.headline).frame(maxWidth: .infinity).padding(.vertical, 6)
                    }
                    .buttonStyle(.bordered)
                }
            }
            .padding(16)
        }
        .navigationBarTitleDisplayMode(.inline)
        .pulseScreen()
    }
}
