import AVFoundation
import SwiftUI
import UIKit

// QR scanner (AVFoundation, no third-party dependencies). Expects the
// server's pairing payload: https://<lan-ip>:<port>#<pin> (PIN in the hash
// fragment, never sent to the server). See src/server/qr.rs.
struct QRScannerView: UIViewControllerRepresentable {
    var onCode: (String) -> Void
    var onCancel: () -> Void

    func makeUIViewController(context: Context) -> ScannerViewController {
        ScannerViewController(onCode: onCode, onCancel: onCancel)
    }

    func updateUIViewController(_ uiViewController: ScannerViewController, context: Context) {}
}

final class ScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    private let onCode: (String) -> Void
    private let onCancel: () -> Void
    private var session: AVCaptureSession?
    private var didFire = false

    init(onCode: @escaping (String) -> Void, onCancel: @escaping () -> Void) {
        self.onCode = onCode
        self.onCancel = onCancel
        super.init(nibName: nil, bundle: nil)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is unavailable") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black

        let capture = AVCaptureSession()
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              capture.canAddInput(input)
        else { return }
        capture.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard capture.canAddOutput(output) else { return }
        capture.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: DispatchQueue.main)
        output.metadataObjectTypes = [.qr]

        let preview = AVCaptureVideoPreviewLayer(session: capture)
        preview.videoGravity = .resizeAspectFill
        preview.frame = view.bounds
        preview.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        view.layer.addSublayer(preview)

        let cancel = UIButton(type: .system)
        cancel.setTitle("Cancel", for: .normal)
        cancel.setTitleColor(.white, for: .normal)
        cancel.backgroundColor = UIColor(white: 0, alpha: 0.5)
        cancel.layer.cornerRadius = 8
        cancel.translatesAutoresizingMaskIntoConstraints = false
        cancel.addTarget(self, action: #selector(cancelTapped), for: .touchUpInside)
        view.addSubview(cancel)
        NSLayoutConstraint.activate([
            cancel.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -24),
            cancel.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            cancel.widthAnchor.constraint(equalToConstant: 120),
            cancel.heightAnchor.constraint(equalToConstant: 44),
        ])

        session = capture
        DispatchQueue.global(qos: .userInitiated).async { capture.startRunning() }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        session?.stopRunning()
    }

    @objc private func cancelTapped() {
        onCancel()
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !didFire,
              let code = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              let value = code.stringValue, !value.isEmpty
        else { return }
        didFire = true
        session?.stopRunning()
        onCode(value)
    }
}

// Parses the pairing QR payload into host/port/PIN.
enum QRParser {
    struct Parsed {
        let host: String
        let port: Int
        let pin: String
    }

    static func parse(_ text: String) -> Parsed? {
        guard let components = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)),
              components.scheme == "https",
              let host = components.host, !host.isEmpty,
              let pin = components.fragment, !pin.isEmpty
        else { return nil }
        return Parsed(host: host, port: components.port ?? 8443, pin: pin)
    }
}
