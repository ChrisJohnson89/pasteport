//
//  PasteportEngine.swift
//  Swift wrapper over the Rust engine's C ABI.
//
//  The protocol types are modelled here as Codable rather than crossing the FFI
//  boundary as structs. That keeps the C surface at four functions and means
//  adding a request never touches the Rust side's header.
//

import Foundation

// MARK: - Protocol types

/// Mirrors `pasteport_core::ClipKind`.
enum ClipKind: String, Codable, CaseIterable {
    case text
    case richText = "rich_text"
    case link
    case color
    case image
    case file

    var symbolName: String {
        switch self {
        case .text: return "text.alignleft"
        case .richText: return "textformat"
        case .link: return "link"
        case .color: return "eyedropper.halffull"
        case .image: return "photo"
        case .file: return "doc"
        }
    }
}

/// Mirrors `pasteport_core::Clip`.
struct Clip: Codable, Identifiable, Hashable {
    let id: Int64
    let kind: ClipKind
    let mime: String
    let text: String?
    let byteLen: Int64
    let sourceApp: String?
    let sourceBundleId: String?
    let hash: String
    let createdAt: Int64
    let lastUsedAt: Int64
    let useCount: Int64
    let pinned: Bool

    enum CodingKeys: String, CodingKey {
        case id, kind, mime, text, hash, pinned
        case byteLen = "byte_len"
        case sourceApp = "source_app"
        case sourceBundleId = "source_bundle_id"
        case createdAt = "created_at"
        case lastUsedAt = "last_used_at"
        case useCount = "use_count"
    }

    /// Single-line preview, whitespace collapsed. Mirrors `Clip::preview`.
    var preview: String {
        guard let text, !text.isEmpty else {
            return kind == .image ? "Image (\(byteLen) bytes)" : "\(kind.rawValue) (\(byteLen) bytes)"
        }
        return text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    var lastUsed: Date { Date(timeIntervalSince1970: TimeInterval(lastUsedAt)) }
}

/// Mirrors `pasteport_core::Pinboard`.
struct Pinboard: Codable, Identifiable, Hashable {
    let id: Int64
    let name: String
    let position: Int64
    let clipCount: Int64

    enum CodingKeys: String, CodingKey {
        case id, name, position
        case clipCount = "clip_count"
    }
}

/// Mirrors `pasteport_core::Stats`.
struct Stats: Codable, Hashable {
    let totalClips: Int64
    let pinnedClips: Int64
    let pinboards: Int64
    let totalBytes: Int64
    let oldestCreatedAt: Int64?
    let fullTextSearch: Bool

    enum CodingKeys: String, CodingKey {
        case totalClips = "total_clips"
        case pinnedClips = "pinned_clips"
        case pinboards
        case totalBytes = "total_bytes"
        case oldestCreatedAt = "oldest_created_at"
        case fullTextSearch = "full_text_search"
    }
}

/// Mirrors `pasteport_daemon::StatusReport`.
struct StatusReport: Codable, Hashable {
    let version: String
    let backend: String
    let uptimeSecs: UInt64
    let pollIntervalMs: UInt64
    let license: String
    let licensed: Bool
    let licenseNeedsAttention: Bool
    let stats: Stats
    let dataDir: String

    enum CodingKeys: String, CodingKey {
        case version, backend, license, licensed, stats
        case uptimeSecs = "uptime_secs"
        case pollIntervalMs = "poll_interval_ms"
        case licenseNeedsAttention = "license_needs_attention"
        case dataDir = "data_dir"
    }
}

// MARK: - Errors

enum EngineError: LocalizedError {
    /// The daemon is not listening.
    case notRunning(socket: String)
    /// The daemon replied with an error.
    case service(String)
    /// The reply did not match what the request should produce.
    case unexpectedResponse(String)
    case encoding(String)

    var errorDescription: String? {
        switch self {
        case .notRunning(let socket):
            return "The Pasteport service is not running.\nStart it with: pasteportd\n(\(socket))"
        case .service(let message):
            return message
        case .unexpectedResponse(let detail):
            return "Unexpected reply from the Pasteport service: \(detail)"
        case .encoding(let detail):
            return "Could not encode a request: \(detail)"
        }
    }
}

// MARK: - Engine

/// Talks to `pasteportd` through the Rust FFI layer.
///
/// An actor because a `PasteportClient*` is not thread safe, and SwiftUI will
/// happily call this from several tasks at once.
actor PasteportEngine {
    private var client: OpaquePointer?
    private let socketPath: String

    /// Engine version reported by the linked Rust library.
    static var version: String {
        String(cString: pasteport_version())
    }

    static var defaultSocketPath: String {
        guard let raw = pasteport_default_socket_path() else { return "" }
        defer { pasteport_string_free(raw) }
        return String(cString: raw)
    }

    init(socketPath: String? = nil) {
        self.socketPath = socketPath ?? PasteportEngine.defaultSocketPath
    }

    deinit {
        if let client {
            pasteport_client_free(client)
        }
    }

    // MARK: Requests

    func status() async throws -> StatusReport {
        let response = try await send(["op": "status"])
        guard case .status(let report) = response else {
            throw EngineError.unexpectedResponse("expected status")
        }
        return report
    }

    /// Recent clips, or search results when `query` is non-empty.
    func clips(matching query: String = "", limit: Int = 200) async throws -> [Clip] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let body: [String: Any] = trimmed.isEmpty
            ? ["op": "list", "limit": limit, "offset": 0]
            : ["op": "search", "query": trimmed, "limit": limit]

        let response = try await send(body)
        guard case .clips(let clips) = response else {
            throw EngineError.unexpectedResponse("expected clips")
        }
        return clips
    }

    func clips(ofKind kind: ClipKind, limit: Int = 200) async throws -> [Clip] {
        let response = try await send(["op": "list", "limit": limit, "kind": kind.rawValue])
        guard case .clips(let clips) = response else {
            throw EngineError.unexpectedResponse("expected clips")
        }
        return clips
    }

    func pinboards() async throws -> [Pinboard] {
        let response = try await send(["op": "pinboards"])
        guard case .pinboards(let boards) = response else {
            throw EngineError.unexpectedResponse("expected pinboards")
        }
        return boards
    }

    func clips(inPinboard name: String, limit: Int = 200) async throws -> [Clip] {
        let response = try await send(["op": "pinboard_clips", "name": name, "limit": limit])
        guard case .clips(let clips) = response else {
            throw EngineError.unexpectedResponse("expected clips")
        }
        return clips
    }

    /// Put a stored clip back on the system clipboard.
    @discardableResult
    func copy(id: Int64) async throws -> Clip {
        let response = try await send(["op": "copy", "id": id])
        guard case .clip(let clip) = response else {
            throw EngineError.unexpectedResponse("expected the clip")
        }
        return clip
    }

    func setPinned(id: Int64, pinned: Bool) async throws {
        _ = try await send(["op": "pin", "id": id, "pinned": pinned])
    }

    func delete(id: Int64) async throws {
        _ = try await send(["op": "delete", "id": id])
    }

    func installLicense(key: String) async throws {
        _ = try await send(["op": "license_install", "key": key])
    }

    /// Raw payload of a binary clip.
    func bytes(id: Int64) async throws -> Data? {
        let response = try await send(["op": "get_bytes", "id": id])
        guard case .bytes(let base64) = response else {
            throw EngineError.unexpectedResponse("expected bytes")
        }
        guard let base64 else { return nil }
        return Data(base64Encoded: base64)
    }

    // MARK: Transport

    /// Decoded shape of a protocol response.
    private enum DecodedResponse {
        case ok
        case pong(String)
        case status(StatusReport)
        case clips([Clip])
        case clip(Clip)
        case bytes(String?)
        case pinboards([Pinboard])
        case count(Int)
    }

    private func send(_ body: [String: Any]) async throws -> DecodedResponse {
        let requestData: Data
        do {
            requestData = try JSONSerialization.data(withJSONObject: body)
        } catch {
            throw EngineError.encoding(error.localizedDescription)
        }
        guard let requestJSON = String(data: requestData, encoding: .utf8) else {
            throw EngineError.encoding("request was not valid UTF-8")
        }

        let responseJSON = try withConnectedClient { client in
            guard let raw = pasteport_client_request(client, requestJSON) else {
                throw EngineError.unexpectedResponse("the engine returned nothing")
            }
            defer { pasteport_string_free(raw) }
            return String(cString: raw)
        }

        return try decode(responseJSON)
    }

    /// Run `work` with a live connection, reconnecting once if the daemon was
    /// restarted since the last call.
    private func withConnectedClient<T>(_ work: (OpaquePointer) throws -> T) throws -> T {
        if client == nil {
            client = pasteport_client_connect(socketPath)
        }
        guard let existing = client else {
            throw EngineError.notRunning(socket: socketPath)
        }

        do {
            return try work(existing)
        } catch {
            // The socket may have gone away; drop it and try once more.
            pasteport_client_free(existing)
            client = pasteport_client_connect(socketPath)
            guard let retry = client else {
                throw EngineError.notRunning(socket: socketPath)
            }
            return try work(retry)
        }
    }

    private func decode(_ json: String) throws -> DecodedResponse {
        guard let data = json.data(using: .utf8) else {
            throw EngineError.unexpectedResponse("reply was not valid UTF-8")
        }
        let decoder = JSONDecoder()

        // The tag tells us which payload to expect.
        struct Tag: Decodable { let result: String }
        let tag = try decoder.decode(Tag.self, from: data)

        switch tag.result {
        case "ok":
            return .ok
        case "error":
            struct Body: Decodable { let message: String }
            throw EngineError.service(try decoder.decode(Body.self, from: data).message)
        case "pong":
            struct Body: Decodable { let version: String }
            return .pong(try decoder.decode(Body.self, from: data).version)
        case "status":
            // `Response::Status` is a newtype variant, so the report's fields sit
            // alongside `result` rather than nested under a key.
            return .status(try decoder.decode(StatusReport.self, from: data))
        case "clips":
            struct Body: Decodable { let clips: [Clip] }
            return .clips(try decoder.decode(Body.self, from: data).clips)
        case "clip":
            struct Body: Decodable { let clip: Clip }
            return .clip(try decoder.decode(Body.self, from: data).clip)
        case "bytes":
            struct Body: Decodable { let base64: String? }
            return .bytes(try decoder.decode(Body.self, from: data).base64)
        case "pinboards":
            struct Body: Decodable { let pinboards: [Pinboard] }
            return .pinboards(try decoder.decode(Body.self, from: data).pinboards)
        case "count":
            struct Body: Decodable { let count: Int }
            return .count(try decoder.decode(Body.self, from: data).count)
        default:
            throw EngineError.unexpectedResponse("unknown result \(tag.result)")
        }
    }
}
