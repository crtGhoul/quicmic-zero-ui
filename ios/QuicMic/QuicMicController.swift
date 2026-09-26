import AVFoundation
import Foundation
import UIKit

// Orchestrates pairing, streaming, and reconnection. Mirrors the web client's
// lifecycle from AGENTS.md: renew-before-connect (F5 handover), 1 s /api/stats
// liveness polling, 401 -> renew -> re-pair, 503/server-gone -> drop back to
// pairing (the cert may have regenerated, so the old pin is discarded).
final class QuicMicController: ObservableObject {
    enum Phase {
        case setup        // enter host/PIN or scan QR
        case confirming   // user confirms the cert fingerprint
        case pairing      // /api/pair in flight
        case ready        // paired; tap the mic to stream
        case streaming
    }

    @Published var phase: Phase = .setup
    @Published var host = ""
    @Published var portText = "8443"
    @Published var pin = ""
    @Published var status = ""
    @Published var busy = false
    @Published var showScanner = false
    @Published var fingerprint = ""
    @Published var infoHashMatches = false
    @Published var statsLine = ""
    @Published var level: Float = 0
    @Published var isMuted = false
    @Published var micName: String?
    @Published var speakerLive = false

    private let store = SessionStore()
    private let audio = AudioEngine()
    private var api: ServerAPI?
    private var socketSession: URLSession?
    private var micTask: URLSessionWebSocketTask?
    private var speakerTask: URLSessionWebSocketTask?
    private var token: String?
    private var pinnedHash: Data?
    private var presentedHash: Data?
    private var statsTimer: Timer?
    private var userStopped = false
    private var reconnectAttempts = 0
    private var port: Int { Int(portText.trimmingCharacters(in: .whitespaces)) ?? 8443 }

    init() {
        if let identity = store.identity {
            host = identity.host
            portText = String(identity.port)
            status = "Saved server \(identity.host). Enter the PIN to pair."
        }
        audio.onMicFrame = { [weak self] frame in
            self?.micTask?.send(.data(frame)) { _ in }
        }
        audio.onLevel = { [weak self] value in
            self?.level = value
        }
        audio.onInterruption = { [weak self] began in
            self?.status = began
                ? "Microphone interrupted (call or another app). Waiting for it to return."
                : "Microphone returned."
        }
    }

    // MARK: - Pairing

    private func makeURL(scheme: String, path: String, query: [URLQueryItem]) -> URL? {
        var comps = URLComponents()
        comps.scheme = scheme
        comps.host = host.trimmingCharacters(in: .whitespaces)
        comps.port = port
        comps.path = path
        comps.queryItems = query
        return comps.url
    }

    // Step 1: fetch /api/info over a bootstrap session and show the server's
    // certificate fingerprint for the user to confirm against the PC.
    func beginPairing() {
        let cleanHost = host.trimmingCharacters(in: .whitespaces)
        guard !cleanHost.isEmpty else { status = "Enter the server address."; return }
        busy = true
        // Fast path: a saved identity for this host means the user already
        // confirmed this exact certificate; pin strictly and renew. No PIN
        // needed — the stored session token is the credential.
        if let saved = store.identity, saved.host == cleanHost, saved.port == port,
           let hash = Data(base64Encoded: saved.certHashBase64),
           let savedToken = store.loadToken()
        {
            pinnedHash = hash
            token = savedToken
            api = ServerAPI(host: cleanHost, port: port, pinnedHash: hash)
            Task { await self.fastPathRenew() }
            return
        }
        guard !pin.isEmpty else {
            busy = false
            status = "Enter the PIN shown on the PC."
            return
        }
        status = "Contacting server..."
        api = ServerAPI(host: cleanHost, port: port, pinnedHash: nil)
        Task { await self.bootstrapInfo() }
    }

    @MainActor
    private func fastPathRenew() async {
        defer { busy = false }
        guard let api, let token else { return }
        let cleanHost = host.trimmingCharacters(in: .whitespaces)
        do {
            let newToken = try await api.renew(token: token)
            self.token = newToken
            store.saveToken(newToken)
            socketSession = api.socketSession(pinnedHash: pinnedHash!)
            micName = nil
            phase = .ready
            status = "Session renewed. Tap the mic to stream."
        } catch {
            // Stale token or changed cert: drop back to a bootstrap session
            // so the (possibly new) fingerprint can be confirmed, then pair.
            self.token = nil
            store.deleteToken()
            self.api = ServerAPI(host: cleanHost, port: port, pinnedHash: nil)
            await bootstrapInfo()
        }
    }

    @MainActor
    private func bootstrapInfo() async {
        defer { busy = false }
        guard let api else { return }
        do {
            let info = try await api.fetchInfo()
            guard let presented = api.presentedCertHash else {
                status = "Could not read the server certificate."
                return
            }
            presentedHash = presented
            fingerprint = ServerAPI.fingerprint(presented)
            // Cross-check: the hash we saw on the wire must equal /api/info's
            // cert_hash (base64). A mismatch means something intercepted TLS.
            if let advertised = Data(base64Encoded: info.cert_hash) {
                infoHashMatches = (advertised == presented)
            }
            phase = .confirming
            status = infoHashMatches
                ? "Confirm this fingerprint matches the PC, then pair."
                : "Warning: the certificate does not match /api/info. Only continue on a trusted network."
        } catch {
            status = "Could not reach the server: \(error.localizedDescription)"
        }
    }

    // Step 2 (user confirmed): store the pin, then pair with the PIN.
    func confirmFingerprintAndPair() {
        guard let presented = presentedHash else { return }
        pinnedHash = presented
        let cleanHost = host.trimmingCharacters(in: .whitespaces)
        store.identity = ServerIdentity(
            host: cleanHost,
            port: port,
            certHashBase64: presented.base64EncodedString(),
            deviceName: UIDevice.current.name
        )
        api = ServerAPI(host: cleanHost, port: port, pinnedHash: presented)
        pair()
    }

    func cancelConfirm() {
        phase = .setup
        status = "Pairing cancelled."
    }

    private func pair() {
        phase = .pairing
        busy = true
        status = "Pairing..."
        Task { await self.doPair() }
    }

    @MainActor
    private func doPair() async {
        defer { busy = false }
        guard let api else { return }
        do {
            let response = try await api.pair(pin: pin, deviceName: UIDevice.current.name)
            token = response.token
            store.saveToken(response.token!)
            micName = response.mic_name
            socketSession = api.socketSession(pinnedHash: pinnedHash!)
            phase = .ready
            status = "Paired\(micName.map { " as \($0)" } ?? ""). Tap the mic to stream."
        } catch let error as APIError {
            switch error {
            case .pairFailed(let message):
                // Wrong PIN arrives as HTTP 200 success:false by design.
                status = message
            case .http(429):
                status = "Too many attempts. Wait 30 seconds and try again."
            default:
                status = "Pairing failed: \(error.localizedDescription)"
            }
            phase = .setup
        } catch {
            status = "Pairing failed: \(error.localizedDescription)"
            phase = .setup
        }
    }

    // MARK: - Streaming

    func toggleStream() {
        if phase == .streaming {
            disconnect(userInitiated: true)
        } else if phase == .ready {
            connect()
        }
    }

    private func connect() {
        guard let token, let socketSession else { status = "Pair first."; return }
        busy = true
        status = "Connecting..."
        userStopped = false
        reconnectAttempts = 0
        // Renew first: kicks any stale session and smooths the handover,
        // exactly like the web client's reconnect path.
        Task { await self.renewAndOpen(oldToken: token, session: socketSession) }
    }

    @MainActor
    private func renewAndOpen(oldToken: String, session: URLSession) async {
        defer { busy = false }
        guard let api, let fresh = try? await api.renew(token: oldToken) else {
            status = "Session expired. Pair again."
            phase = .setup
            return
        }
        token = fresh
        store.saveToken(fresh)
        openSockets(token: fresh, session: session)
    }

    private func openSockets(token: String, session: URLSession) {
        guard let micURL = makeURL(scheme: "wss", path: "/ws",
                                   query: [URLQueryItem(name: "token", value: token),
                                           URLQueryItem(name: "sr", value: "48000")])
        else { status = "Invalid server address."; return }

        micTask = session.webSocketTask(with: micURL)
        micTask?.resume()
        pumpMic()

        // Speaker is best-effort: if the PC's Speaker tab is off, the server
        // answers 503 and we simply stream mic-only.
        if let speakerURL = makeURL(scheme: "wss", path: "/speaker-ws",
                                    query: [URLQueryItem(name: "token", value: token)]) {
            speakerTask = session.webSocketTask(with: speakerURL)
            speakerTask?.resume()
            pumpSpeaker()
        }

        AVAudioApplication.requestRecordPermission { [weak self] granted in
            DispatchQueue.main.async {
                guard let self else { return }
                guard granted else {
                    self.status = "Microphone permission denied."
                    self.disconnect(userInitiated: true)
                    return
                }
                do {
                    try self.audio.start()
                    self.phase = .streaming
                    self.status = "Streaming to PC."
                    self.startStatsTimer()
                } catch {
                    self.status = "Audio failed: \(error.localizedDescription)"
                    self.disconnect(userInitiated: true)
                }
            }
        }
    }

    private func pumpMic() {
        micTask?.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case .failure, .success:
                // The server never sends on the mic socket; any close or
                // error means the transport died.
                if self.phase == .streaming, !self.userStopped {
                    self.scheduleReconnect()
                }
            }
        }
    }

    private func pumpSpeaker() {
        speakerTask?.receive { [weak self] result in
            guard let self else { return }
            switch result {
            case .success(.data(let data)):
                if data.count == AudioEngine.speakerFrameBytes {
                    self.audio.playSpeakerFrame(data)
                    if !self.speakerLive {
                        DispatchQueue.main.async { self.speakerLive = true }
                    }
                }
                self.pumpSpeaker()
            case .success(.string):
                self.pumpSpeaker()
            case .failure:
                // Speaker unavailable (e.g. 503: PC capture off). Mic continues.
                DispatchQueue.main.async { self.speakerLive = false }
            default:
                self.pumpSpeaker()
            }
        }
    }

    private func scheduleReconnect() {
        guard phase == .streaming, !userStopped else { return }
        reconnectAttempts += 1
        guard reconnectAttempts <= 5 else {
            status = "Connection lost. Tap the mic to retry."
            disconnect(userInitiated: true)
            return
        }
        let delay = [0, 1, 2, 3, 4][min(reconnectAttempts - 1, 4)]
        status = "Reconnecting..."
        DispatchQueue.main.asyncAfter(deadline: .now() + Double(delay)) { [weak self] in
            guard let self, self.phase == .streaming, !self.userStopped,
                  let token = self.token, let session = self.socketSession
            else { return }
            Task { await self.renewAndOpen(oldToken: token, session: session) }
        }
    }

    // MARK: - Stats / liveness

    private func startStatsTimer() {
        statsTimer?.invalidate()
        statsTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            self?.pollStats()
        }
    }

    private func pollStats() {
        guard let api, let token, phase == .streaming else { return }
        Task {
            do {
                let stats = try await api.stats(token: token)
                await MainActor.run {
                    let loss = String(format: "%.2f", stats.loss_percent)
                    self.statsLine = "\(stats.packets_received) pkts · loss \(loss)% · buf \(stats.buffer_ms) ms"
                    if !stats.audio_device_ok {
                        self.status = "PC audio device lost. Waiting for it to return."
                    }
                }
            } catch let error as APIError {
                await MainActor.run { self.handleStatsError(error) }
            } catch {
                // Transient network blip; the reconnect path handles real loss.
            }
        }
    }

    private func handleStatsError(_ error: APIError) {
        switch error {
        case .serverGone:
            handleServerGone()
        case .unauthorized:
            // Session taken over or token expired: renew once, else re-pair.
            guard let api, let token else { return }
            Task {
                if let fresh = try? await api.renew(token: token) {
                    await MainActor.run {
                        self.token = fresh
                        self.store.saveToken(fresh)
                    }
                } else {
                    await MainActor.run {
                        self.status = "Session expired. Pair again."
                        self.disconnect(userInitiated: true)
                        self.phase = .setup
                    }
                }
            }
        default:
            break
        }
    }

    private func handleServerGone() {
        // The server restarted, so its self-signed cert (and our pin) may be
        // stale — drop the saved identity and force a fresh confirmation,
        // mirroring the web client's reload requirement.
        store.identity = nil
        store.deleteToken()
        token = nil
        pinnedHash = nil
        disconnect(userInitiated: true)
        phase = .setup
        status = "Server is gone (restarted?). Pair again to continue."
    }

    func disconnect(userInitiated: Bool) {
        userStopped = userInitiated
        statsTimer?.invalidate()
        statsTimer = nil
        micTask?.cancel(with: .goingAway, reason: nil)
        speakerTask?.cancel(with: .goingAway, reason: nil)
        micTask = nil
        speakerTask = nil
        audio.stop()
        speakerLive = false
        level = 0
        if userInitiated, phase == .streaming {
            phase = token == nil ? .setup : .ready
            if phase == .ready { status = "Disconnected. Tap the mic to stream." }
        }
    }

    func toggleMute() {
        isMuted.toggle()
        audio.isMuted = isMuted
    }

    // MARK: - QR

    func ensureCameraPermission(completion: @escaping (Bool) -> Void) {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            completion(true)
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video, completionHandler: completion)
        default:
            completion(false)
        }
    }

    func scanned(_ text: String) {
        showScanner = false
        guard let parsed = QRParser.parse(text) else {
            status = "That QR code is not a QuicMic pairing code."
            return
        }
        host = parsed.host
        portText = String(parsed.port)
        pin = parsed.pin
        status = "QR scanned. Tap Pair."
    }
}
