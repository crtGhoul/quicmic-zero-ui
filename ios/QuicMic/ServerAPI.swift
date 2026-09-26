import CryptoKit
import Foundation

// MARK: - Wire models (match the Rust server's JSON exactly)

// GET /api/info (no auth)
struct ServerInfo: Decodable {
    let cert_hash: String
    let wt_port: Int
    let lan_ip: String
    let update_available: Bool?
    let latest_version: String?
    let releases_url: String?
}

// POST /api/pair {"pin","device_name"} -> this. Wrong PIN yields HTTP 200
// with success:false (by design); 429 means the per-IP lockout tripped.
struct PairResponse: Decodable {
    let success: Bool
    let token: String?
    let error: String?
    let mic_name: String?
}

struct PairRequest: Encodable {
    let pin: String
    let device_name: String
}

// POST /api/renew {"token": old} -> this. The old token dies immediately.
struct RenewResponse: Decodable {
    let success: Bool
    let token: String?
    let mic_name: String?
}

struct RenewRequest: Encodable {
    let token: String
}

// GET /api/stats (header X-Session-Token). 503 while the server shuts down,
// 401 when our session was taken over or the token expired.
struct StatsResponse: Decodable {
    let packets_received: UInt64
    let packets_lost: UInt64
    let loss_percent: Double
    let buffer_ms: UInt64
    let connected: Bool
    let audio_device_ok: Bool
}

// MARK: - Certificate pinning

// Trusts ONLY the server's pinned self-signed certificate. In bootstrap mode
// (pinnedHash == nil) the presented certificate is captured for fingerprint
// display but the connection still completes, so /api/info can be fetched;
// the user must visually confirm the fingerprint before it is ever stored.
// After a pin exists, any certificate that does not hash to it is rejected.
final class PinningDelegate: NSObject, URLSessionDelegate {
    var pinnedHash: Data?
    private var _capturedHash: Data?
    private let lock = NSLock()
    var capturedHash: Data? {
        lock.lock()
        defer { lock.unlock() }
        return _capturedHash
    }

    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust,
              let cert = SecTrustGetCertificateAtIndex(trust, 0)
        else {
            completionHandler(.cancelAuthenticationChallenge, nil)
            return
        }
        let der = SecCertificateCopyData(cert) as Data
        let hash = Data(SHA256.hash(data: der))
        lock.lock()
        _capturedHash = hash
        lock.unlock()
        if let pinned = pinnedHash {
            if pinned == hash {
                completionHandler(.useCredential, URLCredential(trust: trust))
            } else {
                completionHandler(.cancelAuthenticationChallenge, nil)
            }
        } else {
            // Bootstrap: pairing-time only. The fingerprint is shown to the
            // user and stored only after explicit confirmation.
            completionHandler(.useCredential, URLCredential(trust: trust))
        }
    }
}

// MARK: - API client

enum APIError: Error, LocalizedError {
    case badURL
    case http(Int)
    case pairFailed(String)
    case certMismatch
    case serverGone
    case unauthorized

    var errorDescription: String? {
        switch self {
        case .badURL: return "Invalid server address."
        case .http(let code): return "Server returned HTTP \(code)."
        case .pairFailed(let msg): return msg
        case .certMismatch: return "Server certificate changed. Re-pair to continue."
        case .serverGone: return "Server unreachable."
        case .unauthorized: return "Session expired."
        }
    }
}

final class ServerAPI {
    let host: String
    let port: Int
    let delegate: PinningDelegate
    private let session: URLSession

    init(host: String, port: Int, pinnedHash: Data?) {
        self.host = host
        self.port = port
        self.delegate = PinningDelegate()
        self.delegate.pinnedHash = pinnedHash
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 5
        config.timeoutIntervalForResource = 10
        self.session = URLSession(configuration: config, delegate: delegate, delegateQueue: nil)
    }

    // Bracket IPv6 literals per RFC 3986 (matches the server's url_host).
    private var baseURL: String {
        let h = (host.contains(":") && !host.hasPrefix("[")) ? "[\(host)]" : host
        return "https://\(h):\(port)"
    }

    var presentedCertHash: Data? { delegate.capturedHash }

    private func get(_ path: String, token: String?) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: baseURL + path) else { throw APIError.badURL }
        var request = URLRequest(url: url)
        if let token {
            request.setValue(token, forHTTPHeaderField: "X-Session-Token")
        }
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw APIError.badURL }
        return (data, http)
    }

    private func post<Body: Encodable>(_ path: String, body: Body) async throws -> (Data, HTTPURLResponse) {
        guard let url = URL(string: baseURL + path) else { throw APIError.badURL }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONEncoder().encode(body)
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw APIError.badURL }
        return (data, http)
    }

    // Fetch /api/info. Also captures the presented certificate's SHA-256 so
    // the caller can show a fingerprint and cross-check info.cert_hash.
    func fetchInfo() async throws -> ServerInfo {
        let (data, http) = try await get("/api/info", token: nil)
        guard http.statusCode == 200 else { throw APIError.http(http.statusCode) }
        return try JSONDecoder().decode(ServerInfo.self, from: data)
    }

    func pair(pin: String, deviceName: String) async throws -> PairResponse {
        let (data, http) = try await post("/api/pair", body: PairRequest(pin: pin, device_name: deviceName))
        guard http.statusCode == 200 else { throw APIError.http(http.statusCode) }
        let response = try JSONDecoder().decode(PairResponse.self, from: data)
        guard response.success, response.token != nil else {
            throw APIError.pairFailed(response.error ?? "Wrong PIN.")
        }
        return response
    }

    func renew(token: String) async throws -> String {
        let (data, http) = try await post("/api/renew", body: RenewRequest(token: token))
        guard http.statusCode == 200 else { throw APIError.http(http.statusCode) }
        let response = try JSONDecoder().decode(RenewResponse.self, from: data)
        guard response.success, let newToken = response.token else { throw APIError.unauthorized }
        return newToken
    }

    func stats(token: String) async throws -> StatsResponse {
        let (data, http) = try await get("/api/stats", token: token)
        switch http.statusCode {
        case 200:
            return try JSONDecoder().decode(StatsResponse.self, from: data)
        case 401:
            throw APIError.unauthorized
        case 503:
            throw APIError.serverGone
        default:
            throw APIError.http(http.statusCode)
        }
    }

    // A URLSession whose WebSocket tasks pin the given certificate.
    func socketSession(pinnedHash: Data) -> URLSession {
        let delegate = PinningDelegate()
        delegate.pinnedHash = pinnedHash
        let config = URLSessionConfiguration.ephemeral
        return URLSession(configuration: config, delegate: delegate, delegateQueue: nil)
    }

    // "AA:BB:CC:..." fingerprint of a raw SHA-256 digest.
    static func fingerprint(_ hash: Data) -> String {
        hash.map { String(format: "%02X", $0) }.joined(separator: ":")
    }
}
