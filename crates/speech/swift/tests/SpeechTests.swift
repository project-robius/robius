// Host-side regressions for the Swift bridge, driving NativeSpeech.swift's own
// types directly. Note that nothing here opens a microphone or touches a
// permission API, so these are safe to run anywhere macOS builds.

import Foundation
import AVFoundation

private var nativeEvents: [(id: UInt64, kind: Int32, text: String)] = []

// The production bridge links this callback from Rust. The test build traps
// before any permission API, even if the unbundled-launch guard regresses.
@_cdecl("robius_speech_event")
func testNativeSpeechEvent(_ id: UInt64, _ kind: Int32, _ text: UnsafePointer<CChar>?, _ level: Float) {
    precondition(Thread.isMainThread)
    nativeEvents.append((id, kind, text.map(String.init(cString:)) ?? ""))
}

@main
enum SpeechTests {
    static let cases: [(String, () -> Void)] = [
        ("audio notifications defer to the main queue", audioNotificationsDeferToMainQueue),
        ("an unbundled launch is rejected", unbundledLaunchIsRejected),
        ("silence retries are bounded", silenceRetriesAreBounded),
        ("capture buffers audio across a request handoff", captureBuffersAcrossHandoff),
        ("capture stops allocating when a recognizer stalls", captureStopsAllocatingWhenStalled),
        ("capture keeps every buffer under concurrent handoff", captureKeepsEveryBufferConcurrently),
        ("capture binds audio to the current request generation", captureBindsAudioToItsGeneration),
        ("utterance rollover commits the draft", rolloverCommitsTheDraft),
        ("cancelling during rollover emits nothing", cancellingDuringRolloverIsSilent),
        ("a graceful stop replays buffered speech", gracefulStopReplaysBufferedSpeech),
    ]

    static func main() {
        for (name, run) in cases {
            // Flush before running: a precondition failure aborts the process,
            // and piped stdout would otherwise lose the name of the failing case.
            print("  \(name)")
            fflush(stdout)
            run()
        }
        print("Passed \(cases.count) native speech regressions; no microphone used.")
    }

    // MARK: - Notification delivery

    static func audioNotificationsDeferToMainQueue() {
        let name = Notification.Name("RobiusSpeech.NotificationDeliveryTest")
        let source = NSObject()
        let posted = DispatchSemaphore(value: 0)
        var received = 0
        let observer = observeAudioNotification(name, object: source) {
            precondition(Thread.isMainThread, "Audio state must remain on the main thread")
            received += 1
        }
        defer { NotificationCenter.default.removeObserver(observer) }

        DispatchQueue.global().async {
            NotificationCenter.default.post(name: name, object: source)
            posted.signal()
        }
        // Model the main thread waiting for an engine operation. The engine's
        // notification-publishing worker must be able to return independently.
        precondition(posted.wait(timeout: .now() + 2) == .success, "Audio notification blocked its posting thread waiting for the main queue")
        precondition(received == 0, "Observer ran before the main queue could process it")
        drainMainQueue(until: { received == 1 })

        // Even a notification posted on main must defer engine teardown until
        // the posting operation has returned, rather than reentering the engine.
        NotificationCenter.default.post(name: name, object: source)
        precondition(received == 1, "Audio notification ran its handler reentrantly")
        drainMainQueue(until: { received == 2 })
    }

    // MARK: - Privacy and permissions

    static func unbundledLaunchIsRejected() {
        // Reproduce cargo-run's misleading embedded privacy descriptions. TCC
        // can ignore them and attribute access to the parent editor instead.
        precondition(Bundle.main.bundleURL.pathExtension != "app")
        for key in ["NSMicrophoneUsageDescription", "NSSpeechRecognitionUsageDescription"] {
            precondition(Bundle.main.object(forInfoDictionaryKey: key) as? String != nil, "The test executable must embed its privacy descriptions")
        }
        nativeEvents.removeAll()
        "".withCString { startNativeSpeech(42, $0, true) }
        drainMainQueue(until: { !nativeEvents.isEmpty })
        precondition(nativeEvents.count == 1)
        precondition(nativeEvents[0].id == 42 && nativeEvents[0].kind == errPermission)
        precondition(nativeEvents[0].text == "Launch the application from its .app bundle to use speech input.")
    }

    // MARK: - Retry policy

    static func silenceRetriesAreBounded() {
        var retries = SpeechRetryPolicy()
        let noSpeech = NSError(domain: "kAFAssistantErrorDomain", code: 1110)
        precondition(retries.retryDelay(after: noSpeech) == 0.25)
        precondition(retries.retryDelay(after: noSpeech) == 0.5)
        precondition(retries.retryDelay(after: noSpeech) == 1.0)
        precondition(retries.retryDelay(after: noSpeech) == nil, "Silence retries must be bounded")
        retries.madeProgress()
        precondition(retries.retryDelay(after: noSpeech) == 0.25, "Recognized speech resets the retry budget")
        for code in [203, 1100, 1101, 1107, 1700] {
            precondition(retries.retryDelay(after: NSError(domain: "kAFAssistantErrorDomain", code: code)) == nil, "A generic failure is not a documented timeout")
        }
        precondition(retries.retryDelay(after: NSError(domain: "UnrelatedDomain", code: 1110)) == nil)
    }

    // MARK: - Audio capture

    static func captureBuffersAcrossHandoff() {
        let capture = SpeechAudioCapture()
        var samples: [Float] = []
        capture.beginRequest { samples.append($0.floatChannelData![0][0]) }
        precondition(capture.append(audioBuffer(sample: 1)))
        capture.endRequest()
        let reusedBuffer = audioBuffer(sample: 2)
        precondition(capture.append(reusedBuffer))
        reusedBuffer.floatChannelData![0][0] = 99
        precondition(capture.append(audioBuffer(sample: 3)))
        precondition(samples == [1], "Finalizing a task must buffer new audio")
        capture.beginRequest { samples.append($0.floatChannelData![0][0]) }
        precondition(capture.append(audioBuffer(sample: 4)))
        precondition(samples == [1, 2, 3, 4], "Copied rollover audio must precede live audio without reuse corruption")
        capture.endRequest()
        precondition(capture.append(audioBuffer(sample: 5)))
        capture.stop(discardPending: false)
        precondition(capture.hasPendingAudio, "Graceful stop must keep buffered speech for final recognition")
        precondition(capture.append(audioBuffer(sample: 99)))
        capture.beginRequest { samples.append($0.floatChannelData![0][0]) }
        precondition(samples == [1, 2, 3, 4, 5], "Final recognition must replay saved speech but no post-stop audio")
        capture.stop()
    }

    static func captureStopsAllocatingWhenStalled() {
        let bounded = SpeechAudioCapture()
        bounded.beginRequest { _ in }
        bounded.endRequest()
        for _ in 0..<4 { precondition(bounded.append(audioBuffer(sample: 0, frames: 8_000))) }
        precondition(!bounded.append(audioBuffer(sample: 0)), "A stalled recognizer must not allocate unbounded PCM storage")
        precondition(bounded.append(audioBuffer(sample: 0)), "Buffer overflow must report one terminal failure")
        precondition(!bounded.hasPendingAudio)
        bounded.stop()
    }

    static func captureKeepsEveryBufferConcurrently() {
        let concurrent = SpeechAudioCapture()
        var ordered: [Int] = []
        let receive: (AVAudioPCMBuffer) -> Void = { ordered.append(Int($0.floatChannelData![0][0])) }
        concurrent.beginRequest(receive)
        let producerFinished = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            for value in 0..<2_000 { precondition(concurrent.append(audioBuffer(sample: Float(value)))) }
            producerFinished.signal()
        }
        for _ in 0..<200 {
            concurrent.endRequest()
            concurrent.beginRequest(receive)
        }
        precondition(producerFinished.wait(timeout: .now() + 2) == .success)
        concurrent.beginRequest(receive)
        precondition(ordered == Array(0..<2_000), "Concurrent request handoff must preserve every PCM buffer in order")
        concurrent.stop()
    }

    static func captureBindsAudioToItsGeneration() {
        let generations = SpeechAudioCapture()
        var oldRequest: [Float] = []
        var newRequest: [Float] = []
        generations.prepareRequest(10)
        generations.beginRequest(generation: 10) { oldRequest.append($0.floatChannelData![0][0]) }
        precondition(generations.append(audioBuffer(sample: 10)))
        let detached = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            generations.endRequest(ifCurrent: 10)
            precondition(generations.append(audioBuffer(sample: 11)))
            detached.signal()
        }
        // Simulate the UI being blocked while a terminal task callback arrives.
        precondition(detached.wait(timeout: .now() + 2) == .success)
        precondition(oldRequest == [10], "PCM after a terminal callback must buffer without waiting for UI dispatch")
        generations.prepareRequest(20)
        generations.beginRequest(generation: 20) { newRequest.append($0.floatChannelData![0][0]) }
        generations.endRequest(ifCurrent: 10)
        precondition(generations.append(audioBuffer(sample: 12)))
        precondition(newRequest == [11, 12], "A delayed old callback must not detach the active successor")
        generations.endRequest(ifCurrent: 20)
        generations.prepareRequest(30)
        generations.endRequest(ifCurrent: 30)
        generations.beginRequest(generation: 30) { _ in preconditionFailure("An already-completed request must not be rebound") }
        precondition(generations.append(audioBuffer(sample: 13)))
        generations.prepareRequest(40)
        generations.beginRequest(generation: 40) { newRequest.append($0.floatChannelData![0][0]) }
        precondition(newRequest == [11, 12, 13], "Immediate task completion must preserve subsequent PCM for its successor")
        generations.stop()
    }

    // MARK: - Session lifecycle

    static func rolloverCommitsTheDraft() {
        nativeEvents.removeAll()
        let rollover = NativeSpeech(id: 43, locale: "", preferOnDevice: true)
        precondition(rollover.locale == nil, "An unspecified locale must use native default-language selection")
        precondition(rollover.engine == nil, "Creating a session must not initialize microphone hardware")
        rollover.started = true
        rollover.generation = 7
        rollover.lastPartial = "Keep this draft"
        rollover.endUtteranceBeforeLimit(6, finalizationTimeout: 0)
        precondition(!rollover.endingUtterance, "An old utterance timer must not affect its successor")
        rollover.endUtteranceBeforeLimit(7, finalizationTimeout: 0)
        drainMainQueue(until: { !nativeEvents.isEmpty })
        precondition(nativeEvents.count == 1 && nativeEvents[0].kind == 2 && nativeEvents[0].text == "Keep this draft")
        precondition(rollover.generation == 8 && rollover.pendingRestart != nil)
        rollover.stop(cancel: false)
        precondition(rollover.finished && rollover.pendingRestart == nil, "Stopping during backoff must cancel the pending restart")
        precondition(nativeEvents.count == 2 && nativeEvents[1].kind == 4)
    }

    static func cancellingDuringRolloverIsSilent() {
        let before = nativeEvents.count
        let cancelled = NativeSpeech(id: 44, locale: "en-US", preferOnDevice: true)
        cancelled.generation = 1
        cancelled.lastPartial = "Cancelled partial"
        cancelled.endUtteranceBeforeLimit(1, finalizationTimeout: 0)
        cancelled.stop(cancel: true)
        var drained = false
        DispatchQueue.main.async { drained = true }
        drainMainQueue(until: { drained })
        precondition(nativeEvents.count == before, "Cancelled rollover must not emit a transcript or restart")
    }

    static func gracefulStopReplaysBufferedSpeech() {
        nativeEvents.removeAll()
        let stopping = NativeSpeech(id: 45, locale: "", preferOnDevice: true)
        stopping.stopping = true
        stopping.lastPartial = "Previous utterance"
        stopping.capture.beginRequest { _ in }
        stopping.capture.endRequest()
        precondition(stopping.capture.append(audioBuffer(sample: 6)))
        stopping.endCapture(discardPending: false)
        stopping.finishStoppedUtterance()
        precondition(nativeEvents.count == 1 && nativeEvents[0].text == "Previous utterance")
        precondition(!stopping.finished && stopping.pendingRestart != nil, "Queued speech must finish before the stopped event")
        var replayed: [Float] = []
        stopping.capture.beginRequest { replayed.append($0.floatChannelData![0][0]) }
        precondition(replayed == [6])
        stopping.lastPartial = "Buffered final words"
        stopping.finishStoppedUtterance()
        precondition(nativeEvents.map(\.kind) == [2, 2, 4])
        precondition(nativeEvents[1].text == "Buffered final words" && stopping.finished)
    }

    // MARK: - Helpers

    private static func audioBuffer(sample: Float, frames: AVAudioFrameCount = 1) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(standardFormatWithSampleRate: 8_000, channels: 1)!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)!
        buffer.frameLength = frames
        for frame in 0..<Int(frames) { buffer.floatChannelData![0][frame] = sample }
        return buffer
    }

    private static func drainMainQueue(until complete: () -> Bool) {
        let deadline = Date(timeIntervalSinceNow: 2)
        while !complete() && Date() < deadline {
            _ = RunLoop.main.run(mode: .default, before: Date(timeIntervalSinceNow: 0.01))
        }
        precondition(complete(), "Deferred audio notification was never delivered")
    }
}
