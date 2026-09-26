import Foundation
import Security

// Persistent pairing state. Non-secret fields (host, port, pinned cert hash)
// live in UserDefaults; the session token lives in the Keychain.
// The pinned cert hash is the SHA-256 of the server's self-signed certificate
// (base64), cross-checked against GET /api/info -> cert_hash at pairing time.
struct ServerIdentity: Codable {
    var host: String
    var port: Int
    var certHashBase64: String
    var deviceName: String
}

final class SessionStore {
    private static let identityKey = "com.quicmic.serverIdentity"
    private static let tokenService = "com.quicmic.sessionToken"
    private static let tokenAccount = "session-token"

    var identity: ServerIdentity? {
        get {
            guard let data = UserDefaults.standard.data(forKey: Self.identityKey) else { return nil }
            return try? JSONDecoder().decode(ServerIdentity.self, from: data)
        }
        set {
            if let value = newValue, let data = try? JSONEncoder().encode(value) {
                UserDefaults.standard.set(data, forKey: Self.identityKey)
            } else {
                UserDefaults.standard.removeObject(forKey: Self.identityKey)
            }
        }
    }

    // MARK: - Keychain (session token)

    func saveToken(_ token: String) -> Bool {
        deleteToken()
        guard let data = token.data(using: .utf8) else { return false }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.tokenService,
            kSecAttrAccount as String: Self.tokenAccount,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        return SecItemAdd(query as CFDictionary, nil) == errSecSuccess
    }

    func loadToken() -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.tokenService,
            kSecAttrAccount as String: Self.tokenAccount,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    @discardableResult
    func deleteToken() -> Bool {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.tokenService,
            kSecAttrAccount as String: Self.tokenAccount,
        ]
        let status = SecItemDelete(query as CFDictionary)
        return status == errSecSuccess || status == errSecItemNotFound
    }
}
