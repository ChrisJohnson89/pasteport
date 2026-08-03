//
//  DaemonController.swift
//  Starts and stops the bundled pasteportd.
//
//  The app has to be self-sufficient: someone who drags Pasteport into
//  Applications and double-clicks it should get a working clipboard history,
//  with no terminal involved. So the daemon ships inside the bundle and the app
//  launches it on demand.
//
//  Deliberately not a LaunchAgent. A plist in ~/Library/LaunchAgents is state
//  installed outside the bundle that survives dragging the app to the trash,
//  which is a rude thing to leave behind. Spawning a child means uninstalling is
//  still "delete the app".
//

import Foundation

/// Owns the lifetime of the `pasteportd` process the app depends on.
@MainActor
final class DaemonController: ObservableObject {
    /// Whether a daemon is currently reachable on the socket.
    @Published private(set) var isRunning = false
    /// Set when we tried to start it and could not.
    @Published private(set) var startupError: String?

    /// The process we launched, if we launched one. Nil when we attached to a
    /// daemon that was already running, in which case it is not ours to kill.
    private var child: Process?

    private let socketPath: String

    init(socketPath: String = PasteportEngine.defaultSocketPath) {
        self.socketPath = socketPath
    }

    /// Path to `pasteportd` inside the app bundle.
    ///
    /// Also checks a couple of development locations, so running the app from a
    /// build directory works without assembling a bundle first.
    static func bundledDaemonURL() -> URL? {
        // Contents/MacOS/pasteportd — beside the app binary.
        if let exec = Bundle.main.executableURL {
            let sibling = exec.deletingLastPathComponent().appendingPathComponent("pasteportd")
            if FileManager.default.isExecutableFile(atPath: sibling.path) {
                return sibling
            }
        }
        // Contents/Resources/pasteportd.
        if let resource = Bundle.main.url(forResource: "pasteportd", withExtension: nil),
           FileManager.default.isExecutableFile(atPath: resource.path) {
            return resource
        }
        // Development fallback: a cargo target directory near the executable.
        if let exec = Bundle.main.executableURL {
            var dir = exec.deletingLastPathComponent()
            for _ in 0..<6 {
                let candidate = dir.appendingPathComponent("target/release/pasteportd")
                if FileManager.default.isExecutableFile(atPath: candidate.path) {
                    return candidate
                }
                dir = dir.deletingLastPathComponent()
            }
        }
        return nil
    }

    /// Make sure a daemon is reachable, starting the bundled one if not.
    ///
    /// Safe to call repeatedly; it attaches to an existing daemon rather than
    /// starting a second one. That matters because the CLI may already have one
    /// running, and two daemons on one database is exactly the situation the
    /// socket-binding logic refuses to allow.
    func ensureRunning() async {
        if socketAnswers() {
            isRunning = true
            startupError = nil
            return
        }

        guard let daemon = DaemonController.bundledDaemonURL() else {
            isRunning = false
            startupError = """
                Could not find the pasteportd helper inside the app bundle. \
                Reinstall Pasteport, or start the service manually with `pasteportd`.
                """
            return
        }

        let process = Process()
        process.executableURL = daemon
        // Inherit PASTEPORT_DATA_DIR if the user set one, so the app and the CLI
        // agree on which history they are looking at.
        process.environment = ProcessInfo.processInfo.environment
        // The daemon logs to stderr; discard it rather than filling the app's
        // console. Anyone debugging runs pasteportd from a terminal instead.
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice

        do {
            try process.run()
            child = process
        } catch {
            isRunning = false
            startupError = "Could not start the Pasteport service: \(error.localizedDescription)"
            return
        }

        // Wait for it to bind. Two seconds is generous: binding a Unix socket and
        // opening SQLite is milliseconds' work, and the alternative to polling is
        // a fixed sleep that is either too short or wastes launch time.
        for _ in 0..<40 {
            if socketAnswers() {
                isRunning = true
                startupError = nil
                return
            }
            // A child that has already exited will never bind, so stop waiting
            // and report its status. Blaming "readiness" for a process that died
            // on startup sends people looking in the wrong place — the common
            // cause is another daemon already holding the socket.
            if !process.isRunning {
                child = nil
                isRunning = false
                let status = process.terminationStatus
                startupError = "The Pasteport service exited immediately (status \(status)). "
                    + "Run `pasteportd` in a terminal to see why."
                return
            }
            try? await Task.sleep(for: .milliseconds(50))
        }

        isRunning = false
        startupError = "The Pasteport service started but did not become ready."
    }

    /// Stop the daemon, but only if this app started it.
    ///
    /// A daemon someone launched from a terminal keeps running: the app quitting
    /// is not a reason to tear down a service it does not own.
    func stopIfOurs() {
        guard let child, child.isRunning else { return }
        // SIGTERM, which the daemon handles by shutting down cleanly and removing
        // its socket. Terminating without that leaves a stale socket file behind.
        child.terminate()
        // Brief, bounded wait so the socket is gone before we exit. Not
        // waitUntilExit(), which would hang app quit if the daemon wedged.
        for _ in 0..<20 {
            if !child.isRunning { break }
            usleep(50_000)
        }
        self.child = nil
        isRunning = false
    }

    /// True when something is listening on the control socket.
    private func socketAnswers() -> Bool {
        guard let client = pasteport_client_connect(socketPath) else { return false }
        pasteport_client_free(client)
        return true
    }
}
