import AppKit
import AVFoundation
import ScreenCaptureKit
import VideoToolbox

final class Recorder: NSObject, SCStreamOutput {
    let writer: AVAssetWriter
    let input: AVAssetWriterInput
    let outputURL: URL
    var first: CMTime?
    var frames: [Double] = []
    var rejected: [Double] = []
    var appendDurations: [Double] = []
    let lock = NSLock()

    init(url: URL, width: Int, height: Int) throws {
        outputURL = url
        writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.proRes422LT,
            AVVideoWidthKey: width, AVVideoHeightKey: height,
            AVVideoEncoderSpecificationKey: [
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true
            ]
        ])
        input.expectsMediaDataInRealTime = true
        writer.add(input)
        super.init()
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer,
                of type: SCStreamOutputType) {
        guard type == .screen, sample.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
              let status = attachments.first?[.status] as? Int,
              status == SCFrameStatus.complete.rawValue else { return }
        lock.lock()
        defer { lock.unlock() }
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        if first == nil {
            first = pts
            writer.startWriting()
            writer.startSession(atSourceTime: pts)
            try? Data("ready".utf8).write(to: outputURL.appendingPathExtension("ready"))
        }
        let elapsed = CMTimeGetSeconds(pts - first!)
        let before = ProcessInfo.processInfo.systemUptime
        if input.isReadyForMoreMediaData && input.append(sample) {
            frames.append(elapsed)
        } else {
            rejected.append(elapsed)
        }
        appendDurations.append((ProcessInfo.processInfo.systemUptime-before)*1000)
    }

    func finish() async throws {
        input.markAsFinished()
        await writer.finishWriting()
        guard writer.status == .completed else {
            throw writer.error ?? NSError(domain: "Capture", code: 1)
        }
        let data: [String: Any] = ["frames": frames, "writer_rejections": rejected, "append_ms": appendDurations,
                                  "requested_fps": 60, "window_capture": true]
        try JSONSerialization.data(withJSONObject: data, options: .prettyPrinted)
            .write(to: outputURL.appendingPathExtension("timing.json"))
    }
}

@main struct Main {
    @MainActor
    static func main() async throws {
        NSApplication.shared.setActivationPolicy(.prohibited)
        _ = CGMainDisplayID()
        let activity = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .latencyCritical, .idleSystemSleepDisabled],
            reason: "Record the owned Superapp window")
        defer { ProcessInfo.processInfo.endActivity(activity) }
        let args = CommandLine.arguments
        let windowID = CGWindowID(args[1])!
        let url = URL(fileURLWithPath: args[2])
        let seconds = Double(args[3])!
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        guard let window = content.windows.first(where: { $0.windowID == windowID }) else {
            throw NSError(domain: "No owned window", code: 1)
        }
        let config = SCStreamConfiguration()
        config.width = Int(window.frame.width)*2
        config.height = Int(window.frame.height)*2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        config.queueDepth = 8
        config.showsCursor = false
        config.capturesAudio = false
        config.ignoreShadowsSingleWindow = true
        config.shouldBeOpaque = true
        config.scalesToFit = false
        let recorder = try Recorder(url: url, width: config.width, height: config.height)
        let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: window),
                              configuration: config, delegate: nil)
        try stream.addStreamOutput(recorder, type: .screen,
                                   sampleHandlerQueue: DispatchQueue(label: "promo.video", qos: .userInteractive))
        try await stream.startCapture()
        try await Task.sleep(nanoseconds: UInt64(seconds*1_000_000_000))
        try await stream.stopCapture()
        try await recorder.finish()
    }
}
