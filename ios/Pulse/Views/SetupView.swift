import SwiftUI
import UniformTypeIdentifiers

struct SetupView: View {
    @Environment(SnapshotStore.self) private var store
    @State private var picking = false

    var body: some View {
        VStack(spacing: 24) {
            Spacer()
            Image(systemName: "waveform.path.ecg")
                .font(.system(size: 54, weight: .semibold))
                .foregroundStyle(Theme.blue)
            VStack(spacing: 8) {
                Text("Pulse").font(.largeTitle.weight(.bold)).foregroundStyle(Theme.text)
                Text("Your briefing, portfolio and risk on the go.")
                    .font(.subheadline).foregroundStyle(Theme.textSecondary)
            }
            Card {
                VStack(alignment: .leading, spacing: 12) {
                    step(1, "Pulse on your Mac saves a small file, pulse-snapshot.json, to iCloud Drive in a folder called Pulse after every run.")
                    step(2, "Choose that folder once. This app reads the file and keeps itself up to date.")
                    step(3, "It is read-only: it can't trade or change anything.")
                }
            }
            Button { picking = true } label: {
                Text("Choose the Pulse folder").font(.headline).frame(maxWidth: .infinity).padding(.vertical, 6)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            if let error = store.error {
                Text(error).font(.footnote).foregroundStyle(Theme.down).multilineTextAlignment(.center)
            }
            Spacer()
        }
        .padding(.horizontal, 20)
        .background(Theme.bg.ignoresSafeArea())
        .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { result in
            if case let .success(url) = result {
                Task { await store.chooseFolder(url) }
            }
        }
    }

    private func step(_ n: Int, _ text: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            Text("\(n)")
                .font(.caption.weight(.bold))
                .foregroundStyle(Theme.bg)
                .frame(width: 20, height: 20)
                .background(Theme.blue, in: Circle())
            Text(text).font(.subheadline).foregroundStyle(Theme.text).fixedSize(horizontal: false, vertical: true)
        }
    }
}

struct SettingsView: View {
    @Environment(SnapshotStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var picking = false

    var body: some View {
        NavigationStack {
            List {
                Section("Snapshot") {
                    if let s = store.snapshot {
                        row("Made on the Mac", Fmt.timestamp(s.generatedAt).map { $0.formatted(date: .abbreviated, time: .shortened) } ?? s.generatedAt)
                        row("Format", "v\(s.version)")
                    }
                    if let read = store.lastRead {
                        row("Last checked", read.formatted(date: .omitted, time: .shortened))
                    }
                    if let error = store.error {
                        Text(error).foregroundStyle(Theme.down).font(.footnote)
                    }
                }
                Section {
                    Button("Refresh now") { Task { await store.refresh() } }
                    Button("Choose a different folder") { picking = true }
                    Button("Forget the folder", role: .destructive) {
                        store.forgetFolder()
                        dismiss()
                    }
                } footer: {
                    Text("This app only reads the file. It never changes your account.")
                }
            }
            .scrollContentBackground(.hidden)
            .background(Theme.bg)
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .fileImporter(isPresented: $picking, allowedContentTypes: [.folder]) { result in
                if case let .success(url) = result {
                    Task { await store.chooseFolder(url) }
                }
            }
        }
        .preferredColorScheme(.dark)
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack {
            Text(label)
            Spacer()
            Text(value).foregroundStyle(Theme.textSecondary)
        }
    }
}
