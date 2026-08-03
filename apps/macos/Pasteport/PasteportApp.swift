//
//  PasteportApp.swift
//  Menu bar app entry point.
//
//  A MenuBarExtra rather than a dock app: a clipboard manager should be
//  reachable without ever taking focus, and it has no document model to justify
//  a window of its own.
//

import SwiftUI

@main
struct PasteportApp: App {
    @StateObject private var model = HistoryModel()

    var body: some Scene {
        MenuBarExtra("Pasteport", systemImage: "doc.on.clipboard") {
            HistoryView(model: model)
                // Sized to show roughly ten rows without scrolling, which is
                // where the useful part of a clipboard history lives.
                .frame(width: 460, height: 520)
                .task { await model.refresh() }
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(model: model)
                .frame(width: 460)
        }
    }
}

// MARK: - Model

/// Observable state for the menu bar UI.
@MainActor
final class HistoryModel: ObservableObject {
    @Published var clips: [Clip] = []
    @Published var query: String = ""
    @Published var status: StatusReport?
    /// Non-nil when the last operation failed. Shown as a banner rather than an
    /// alert: an unreachable daemon is a state, not an event.
    @Published var errorMessage: String?
    @Published var isLoading = false

    private let engine = PasteportEngine()
    /// Debounces search-as-you-type so each keystroke does not open a socket.
    private var searchTask: Task<Void, Never>?

    var engineVersion: String { PasteportEngine.version }

    func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let fetched = try await engine.clips(matching: query)
            clips = fetched
            errorMessage = nil
        } catch {
            clips = []
            errorMessage = error.localizedDescription
        }
        await refreshStatus()
    }

    func refreshStatus() async {
        do {
            status = try await engine.status()
        } catch {
            status = nil
        }
    }

    /// Re-run the search shortly after typing stops.
    func queryChanged(_ newValue: String) {
        query = newValue
        searchTask?.cancel()
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(120))
            guard !Task.isCancelled else { return }
            await self?.refresh()
        }
    }

    /// Put a clip back on the clipboard. Returns true when it worked.
    @discardableResult
    func copy(_ clip: Clip) async -> Bool {
        do {
            try await engine.copy(id: clip.id)
            await refresh()
            return true
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
    }

    func togglePin(_ clip: Clip) async {
        do {
            try await engine.setPinned(id: clip.id, pinned: !clip.pinned)
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func delete(_ clip: Clip) async {
        do {
            try await engine.delete(id: clip.id)
            await refresh()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    func installLicense(key: String) async -> Bool {
        do {
            try await engine.installLicense(key: key)
            await refreshStatus()
            errorMessage = nil
            return true
        } catch {
            errorMessage = error.localizedDescription
            return false
        }
    }
}
