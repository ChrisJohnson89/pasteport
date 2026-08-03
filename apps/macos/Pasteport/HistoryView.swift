//
//  HistoryView.swift
//  The menu bar panel: search field, results list, footer.
//
//  Search-first by design. The list is keyboard-navigable and Return copies the
//  selection, so the common path never needs the mouse.
//

import SwiftUI

struct HistoryView: View {
    @ObservedObject var model: HistoryModel
    @State private var selection: Clip.ID?
    @FocusState private var searchFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            searchField
            Divider()

            if let message = model.errorMessage {
                banner(message)
            } else if model.clips.isEmpty {
                emptyState
            } else {
                clipList
            }

            Divider()
            footer
        }
        .onAppear { searchFocused = true }
        // Poll while this view is on screen.
        //
        // The daemon captures clips whether or not a window is open, so a view
        // that loaded once shows a history that is stale the moment the user
        // copies anything — which was exactly the first bug this app had: an
        // empty list and "0 clips" while three clips sat in the database.
        //
        // A push notification on the socket would be tidier, and is the right
        // answer eventually. A 1.5s poll of a local Unix socket costs microseconds
        // and needs no protocol change, so it is the right answer now.
        //
        // This `.task` is cancelled automatically when the view disappears, so a
        // closed menu bar panel polls nothing.
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(1500))
                if Task.isCancelled { break }
                await model.refreshIfIdle()
            }
        }
        // Refresh immediately on activation too: waiting up to 1.5s after
        // clicking the menu bar icon reads as lag.
        .onReceive(NotificationCenter.default.publisher(
            for: NSApplication.didBecomeActiveNotification
        )) { _ in
            Task { await model.refreshIfIdle() }
        }
    }

    // MARK: Search

    private var searchField: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .foregroundStyle(.secondary)

            TextField("Search clipboard history", text: Binding(
                get: { model.query },
                set: { model.queryChanged($0) }
            ))
            .textFieldStyle(.plain)
            .focused($searchFocused)
            .onSubmit { copySelectionOrFirst() }

            if !model.query.isEmpty {
                Button {
                    model.queryChanged("")
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundStyle(.secondary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Clear search")
            }
        }
        .padding(10)
    }

    // MARK: List

    private var clipList: some View {
        List(model.clips, selection: $selection) { clip in
            ClipRow(clip: clip)
                .contentShape(Rectangle())
                .onTapGesture(count: 2) {
                    Task { await model.copy(clip) }
                }
                .contextMenu {
                    Button("Copy to Clipboard") {
                        Task { await model.copy(clip) }
                    }
                    Button(clip.pinned ? "Unpin" : "Pin") {
                        Task { await model.togglePin(clip) }
                    }
                    Divider()
                    Button("Delete", role: .destructive) {
                        Task { await model.delete(clip) }
                    }
                }
        }
        .listStyle(.inset)
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: model.query.isEmpty ? "doc.on.clipboard" : "magnifyingglass")
                .font(.system(size: 28))
                .foregroundStyle(.tertiary)
            Text(model.query.isEmpty ? "Nothing copied yet" : "No matches")
                .foregroundStyle(.secondary)
            if model.query.isEmpty {
                Text("Copy something and it will show up here.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    /// Shown when the daemon is unreachable, which is the failure users will
    /// actually hit. Says what to run rather than just what broke.
    private func banner(_ message: String) -> some View {
        VStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle")
                .font(.system(size: 26))
                .foregroundStyle(.orange)
            Text(message)
                .font(.callout)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            // start(), not refresh(): retrying has to be able to launch the
            // service, which is the thing that is usually wrong.
            Button("Try Again") {
                Task { await model.start() }
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    // MARK: Footer

    private var footer: some View {
        HStack(spacing: 8) {
            if let status = model.status {
                Text("\(status.stats.totalClips) clips")
                    .foregroundStyle(.secondary)
            } else {
                Text("Service not running")
                    .foregroundStyle(.orange)
            }

            Spacer()

            SettingsLink {
                Image(systemName: "gearshape")
            }
            .buttonStyle(.plain)
            .help("Settings")

            Button {
                NSApplication.shared.terminate(nil)
            } label: {
                Image(systemName: "power")
            }
            .buttonStyle(.plain)
            .help("Quit Pasteport")
        }
        .font(.caption)
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
    }

    /// Return copies the highlighted row, or the top result when nothing is
    /// highlighted — the usual "type a few letters and hit Return" flow.
    private func copySelectionOrFirst() {
        let target = model.clips.first { $0.id == selection } ?? model.clips.first
        guard let target else { return }
        Task { await model.copy(target) }
    }
}

// MARK: - Row

struct ClipRow: View {
    let clip: Clip

    var body: some View {
        HStack(spacing: 9) {
            Image(systemName: clip.kind.symbolName)
                .frame(width: 16)
                .foregroundStyle(.secondary)

            VStack(alignment: .leading, spacing: 1) {
                Text(clip.preview)
                    .lineLimit(1)
                    .truncationMode(.tail)

                HStack(spacing: 5) {
                    if let app = clip.sourceApp {
                        Text(app)
                    }
                    Text(clip.lastUsed, style: .relative)
                    if clip.useCount > 1 {
                        Text("· used \(clip.useCount)×")
                    }
                }
                .font(.caption2)
                .foregroundStyle(.tertiary)
            }

            Spacer(minLength: 4)

            if clip.pinned {
                Image(systemName: "pin.fill")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
        }
        .padding(.vertical, 2)
    }
}

// MARK: - Settings

struct SettingsView: View {
    @ObservedObject var model: HistoryModel

    var body: some View {
        Form {
            Section("Service") {
                if let status = model.status {
                    LabeledContent("Version", value: status.version)
                    LabeledContent("Clipboard backend", value: status.backend)
                    LabeledContent("Poll interval", value: "\(status.pollIntervalMs) ms")
                    LabeledContent("Search", value: status.stats.fullTextSearch ? "Full text" : "Substring")
                    LabeledContent("Stored clips", value: "\(status.stats.totalClips)")
                    LabeledContent("Data folder", value: status.dataDir)
                        .textSelection(.enabled)
                } else {
                    Text("The Pasteport service is not running. Start it with `pasteportd`.")
                        .foregroundStyle(.secondary)
                }
            }

            Section {
                Text("Settings such as retention and ignored apps live in `config.toml` "
                     + "in the data folder. Editing them here is on the roadmap.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .padding()
        .task { await model.refreshStatus() }
    }
}
