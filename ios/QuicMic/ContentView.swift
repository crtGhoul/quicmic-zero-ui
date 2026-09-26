import AVFoundation
import SwiftUI

struct ContentView: View {
    @EnvironmentObject var controller: QuicMicController

    var body: some View {
        NavigationStack {
            Group {
                switch controller.phase {
                case .setup:
                    SetupView()
                case .confirming:
                    ConfirmView()
                case .pairing:
                    ProgressView("Pairing...")
                case .ready, .streaming:
                    StreamView()
                }
            }
            .navigationTitle("QuicMic")
        }
        .sheet(isPresented: $controller.showScanner) {
            QRScannerView(
                onCode: { controller.scanned($0) },
                onCancel: { controller.showScanner = false }
            )
            .ignoresSafeArea()
        }
    }
}

// MARK: - Pairing

struct SetupView: View {
    @EnvironmentObject var controller: QuicMicController

    var body: some View {
        Form {
            Section("Server") {
                TextField("LAN address, e.g. 192.168.1.42", text: $controller.host)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.numbersAndPunctuation)
                TextField("Port", text: $controller.portText)
                    .keyboardType(.numberPad)
            }
            Section("PIN") {
                TextField("6-digit PIN from the PC", text: $controller.pin)
                    .keyboardType(.numberPad)
            }
            Section {
                Button("Scan QR code") {
                    controller.ensureCameraPermission { granted in
                        DispatchQueue.main.async {
                            if granted {
                                controller.showScanner = true
                            } else {
                                controller.status = "Camera permission denied."
                            }
                        }
                    }
                }
                Button("Pair") { controller.beginPairing() }
                    .disabled(controller.busy)
            }
            if !controller.status.isEmpty {
                Section {
                    Text(controller.status)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .disabled(controller.busy)
    }
}

struct ConfirmView: View {
    @EnvironmentObject var controller: QuicMicController

    var body: some View {
        Form {
            Section("Server certificate") {
                Text("Compare this fingerprint with the one shown on the PC (Diagnostics tab). Only continue if they match.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Text(controller.fingerprint)
                    .font(.system(.footnote, design: .monospaced))
                    .textSelection(.enabled)
                HStack {
                    Text("Matches /api/info:")
                    Spacer()
                    Text(controller.infoHashMatches ? "Yes" : "No")
                        .foregroundStyle(controller.infoHashMatches ? .green : .red)
                        .bold()
                }
            }
            Section {
                Button("Confirm and pair") { controller.confirmFingerprintAndPair() }
                    .disabled(controller.busy)
                Button("Cancel", role: .cancel) { controller.cancelConfirm() }
            }
        }
        .navigationTitle("Confirm certificate")
    }
}

// MARK: - Streaming

struct StreamView: View {
    @EnvironmentObject var controller: QuicMicController

    var body: some View {
        VStack(spacing: 24) {
            Text(controller.micName.map { "Mic: \($0)" } ?? "QuicMic")
                .font(.headline)

            // VU meter
            GeometryReader { geometry in
                ZStack(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 6)
                        .fill(Color.secondary.opacity(0.2))
                    RoundedRectangle(cornerRadius: 6)
                        .fill(controller.isMuted ? Color.yellow : Color.green)
                        .frame(width: geometry.size.width * CGFloat(controller.level))
                }
            }
            .frame(height: 12)
            .padding(.horizontal)

            // Mic button
            Button {
                controller.toggleStream()
            } label: {
                Image(systemName: controller.phase == .streaming ? "mic.fill" : "mic.slash.fill")
                    .font(.system(size: 44))
                    .foregroundStyle(.white)
                    .frame(width: 120, height: 120)
                    .background(controller.phase == .streaming ? Color.green : Color.gray)
                    .clipShape(Circle())
            }
            .disabled(controller.busy)

            Text(controller.phase == .streaming
                 ? (controller.isMuted ? "Muted" : "Streaming")
                 : "Tap the mic to stream")
                .font(.subheadline)
                .foregroundStyle(.secondary)

            HStack(spacing: 32) {
                Button(controller.isMuted ? "Unmute" : "Mute") {
                    controller.toggleMute()
                }
                .disabled(controller.phase != .streaming)

                Button("Disconnect") {
                    controller.disconnect(userInitiated: true)
                }
                .disabled(controller.phase != .streaming)
            }

            if controller.speakerLive {
                Label("PC speaker audio playing", systemImage: "speaker.wave.2.fill")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }

            if !controller.statsLine.isEmpty {
                Text(controller.statsLine)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            }

            if !controller.status.isEmpty {
                Text(controller.status)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal)
            }

            Spacer()
        }
        .padding(.top, 32)
    }
}
