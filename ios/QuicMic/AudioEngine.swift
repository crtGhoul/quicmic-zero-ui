import AVFoundation

// Audio capture + playback for the iOS client.
// Mic: AVAudioEngine tap on the input node, converted to 48 kHz mono Int16,
// framed as 4-byte u32 LE sequence + 480 samples (964 bytes), exactly the
// wire format the server expects on /ws (same as the WebTransport datagrams).
// Speaker: /speaker-ws delivers 7680-byte frames (20 ms stereo f32-LE @48k),
// played through an AVAudioPlayerNode.
final class AudioEngine {
    static let sampleRate: Double = 48_000
    static let frameSamples = 480
    static let micFrameBytes = 4 + frameSamples * 2 // 964
    static let speakerFrameBytes = 7680

    // Called on a private serial queue (never the audio thread) with each
    // complete 964-byte mic frame ready to send over the socket.
    var onMicFrame: ((Data) -> Void)?
    // Throttled (~10 Hz) mic level 0...1 for the VU meter.
    var onLevel: ((Float) -> Void)?

    var isMuted = false

    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private var converter: AVAudioConverter?
    private var micFormat: AVAudioFormat?
    private var speakerFormat: AVAudioFormat?

    private var pending = [Int16]()
    private var sequence: UInt32 = 0
    private let sendQueue = DispatchQueue(label: "com.quicmic.micSend")
    private var lastLevelReport = Date.distantPast
    private var running = false

    // MARK: - Lifecycle

    func start() throws {
        guard !running else { return }
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(
            .playAndRecord,
            mode: .default,
            options: [.allowBluetooth, .allowBluetoothA2DP, .defaultToSpeaker]
        )
        try session.setActive(true)

        guard let inputFormat = engine.inputNode.inputFormat(forBus: 0) as AVAudioFormat?,
              let mic = AVAudioFormat(
                  commonFormat: .pcmFormatInt16,
                  sampleRate: Self.sampleRate,
                  channels: 1,
                  interleaved: true
              ),
              let speaker = AVAudioFormat(
                  commonFormat: .pcmFormatFloat32,
                  sampleRate: Self.sampleRate,
                  channels: 2,
                  interleaved: true
              ),
              let conv = AVAudioConverter(from: inputFormat, to: mic)
        else {
            throw AudioError.setupFailed
        }
        micFormat = mic
        speakerFormat = speaker
        converter = conv

        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: speaker)

        pending.removeAll(keepingCapacity: true)
        sequence = 0

        engine.inputNode.installTap(onBus: 0, bufferSize: 2048, format: inputFormat) { [weak self] buffer, _ in
            self?.handleTap(buffer)
        }

        engine.prepare()
        try engine.start()
        player.play()
        running = true
        observeInterruptions()
    }

    func stop() {
        guard running else { return }
        running = false
        NotificationCenter.default.removeObserver(self)
        player.stop()
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    // MARK: - Mic path

    private func handleTap(_ buffer: AVAudioPCMBuffer) {
        guard running, let converter, let micFormat else { return }
        guard let out = AVAudioPCMBuffer(pcmFormat: micFormat, frameCapacity: 4096) else { return }

        var fed = false
        var convertError: NSError?
        let status = converter.convert(to: out, error: &convertError) { _, outStatus in
            if !fed {
                fed = true
                outStatus.pointee = .haveData
                return buffer
            }
            outStatus.pointee = .noDataNow
            return nil
        }
        guard status != .error, out.frameLength > 0,
              let src = out.int16ChannelData?[0] else { return }

        let count = Int(out.frameLength)
        // Level metering on the converted mono samples.
        var sum: Double = 0
        for i in 0..<count {
            let s = Double(src[i]) / 32768.0
            sum += s * s
        }
        let rms = Float(sqrt(sum / Double(count)))
        let now = Date()
        if now.timeIntervalSince(lastLevelReport) > 0.1 {
            lastLevelReport = now
            let level = isMuted ? 0 : min(1, rms * 3)
            DispatchQueue.main.async { [weak self] in self?.onLevel?(level) }
        }
        guard !isMuted else { return }

        // Accumulate into 480-sample wire frames and hand them off-queue.
        pending.append(contentsOf: UnsafeBufferPointer(start: src, count: count))
        while pending.count >= Self.frameSamples {
            let chunk = Array(pending.prefix(Self.frameSamples))
            pending.removeFirst(Self.frameSamples)
            let seq = sequence
            sequence &+= 1
            var frame = Data(capacity: Self.micFrameBytes)
            var leSeq = seq.littleEndian
            withUnsafeBytes(of: &leSeq) { frame.append(contentsOf: $0) }
            chunk.withUnsafeBytes { frame.append(contentsOf: $0) }
            sendQueue.async { [weak self] in self?.onMicFrame?(frame) }
        }
    }

    // MARK: - Speaker path

    // One 7680-byte server frame -> interleaved stereo f32 @48k -> player.
    func playSpeakerFrame(_ data: Data) {
        guard running, data.count == Self.speakerFrameBytes,
              let format = speakerFormat,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 960),
              let dst = buffer.float32ChannelData?[0]
        else { return }
        buffer.frameLength = 960
        data.withUnsafeBytes { src in
            memcpy(dst, src.baseAddress!, Self.speakerFrameBytes)
        }
        player.scheduleBuffer(buffer)
        if !player.isPlaying {
            player.play()
        }
    }

    // MARK: - Interruptions (calls, other apps taking the mic)

    var onInterruption: ((Bool) -> Void)? // true = began, false = ended

    private func observeInterruptions() {
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(handleInterruption(_:)),
            name: AVAudioSession.interruptionNotification,
            object: nil
        )
    }

    @objc private func handleInterruption(_ note: Notification) {
        guard let info = note.userInfo,
              let raw = info[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }
        DispatchQueue.main.async { [weak self] in
            self?.onInterruption?(type == .began)
        }
    }
}

enum AudioError: Error, LocalizedError {
    case setupFailed
    case permissionDenied

    var errorDescription: String? {
        switch self {
        case .setupFailed: return "Could not configure audio."
        case .permissionDenied: return "Microphone permission denied."
        }
    }
}
