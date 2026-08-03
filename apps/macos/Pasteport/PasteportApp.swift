//
//  PasteportApp.swift
//  App entry point.
//
//  Two surfaces on purpose:
//
//  * A menu bar item, which is how a clipboard manager is actually used.
//  * A real window, which is what makes double-clicking the app in Applications
//    do something visible. A pure LSUIElement menu bar app launches into
//    apparent silence, and "I clicked it and nothing happened" is a worse first
//    experience than a dock icon we do not strictly need.
//

import SwiftUI

@main
struct PasteportApp: App {
    @StateObject private var model = HistoryModel()
    /// Hooks app termination so the bundled daemon is not orphaned.
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    var body: some Scene {
        Window("Pasteport", id: "history") {
            HistoryView(model: model)
                .frame(minWidth: 420, minHeight: 380)
                .task {
                    // Start the service before the first query, so a fresh
                    // install shows history instead of an error banner.
                    appDelegate.model = model
                    await model.start()
                }
        }
        .defaultSize(width: 520, height: 560)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Refresh") {
                    Task { await model.refresh() }
                }
                .keyboardShortcut("r")
            }
        }

        MenuBarExtra("Pasteport", systemImage: "doc.on.clipboard") {
            HistoryView(model: model)
                // Roughly ten rows without scrolling, which is where the useful
                // part of a clipboard history lives.
                .frame(width: 460, height: 520)
                .task { await model.start() }
        }
        .menuBarExtraStyle(.window)

        Settings {
            SettingsView(model: model)
                .frame(width: 480)
        }
    }
}

/// Exists only so the daemon we spawned gets stopped when the app quits.
final class AppDelegate: NSObject, NSApplicationDelegate {
    @MainActor var model: HistoryModel?

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated {
            model?.shutdownService()
        }
    }

    /// Keep running when the window is closed: the menu bar item is the point.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

// MARK: - Model

/// Observable state shared by the window and the menu bar panel.
@MainActor
final class HistoryModel: ObservableObject {
    @Published var clips: [Clip] = []
    @Published var query: String = ""
    @Published var status: StatusReport?
    /// Non-nil when the last operation failed. Shown as a banner rather than an
    /// alert: an unreachable service is a state, not an event.
    @Published var errorMessage: String?
    /// Set by the app when the service could not be started at all.
    @Published var daemonError: String?
    @Published var isLoading = false

    private let engine = PasteportEngine()
    /// Owns the bundled service process.
    private let daemon = DaemonController()
    /// Debounces search-as-you-type so each keystroke does not open a socket.
    private var searchTask: Task<Void, Never>?

    var engineVersion: String { PasteportEngine.version }

    /// Make sure the service is up, then load history.
    ///
    /// This is what both the window and the menu bar panel call, and it is what
    /// the error banner's Retry button calls. Retry has to be able to *start* the
    /// service, not merely re-query it: the failure people actually hit is the
    /// service not running, and a retry that skips this step can never fix it.
    func start() async {
        await daemon.ensureRunning()
        daemonError = daemon.startupError
        await refresh()
    }

    /// Stop the service if this app started it. Called on app termination.
    func shutdownService() {
        daemon.stopIfOurs()
    }

    func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            clips = try await engine.clips(matching: query)
            errorMessage = nil
            daemonError = nil
        } catch {
            clips = []
            errorMessage = daemonError ?? error.localizedDescription
        }
        await refreshStatus()
    }

    func refreshStatus() async {
        status = try? await engine.status()
    }

    /// Re-query only if nothing else is mid-flight.
    ///
    /// Used by the poll loop. Skipping while a load or a debounced search is in
    /// progress keeps the poll from fighting the user's typing.
    func refreshIfIdle() async {
        guard !isLoading, searchTask == nil else { return }
        await refresh()
    }

    /// Re-run the search shortly after typing stops.
    func queryChanged(_ newValue: String) {
        query = newValue
        searchTask?.cancel()
        searchTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(120))
            guard !Task.isCancelled else { return }
            await self?.refresh()
            self?.searchTask = nil
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
}
