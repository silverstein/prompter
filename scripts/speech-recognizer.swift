// Speech helper for Prompter, built on Apple's on-device Speech framework.
//
// LIVE mode (default): recognizes the microphone and streams JSON lines:
//   {"text": "hi thanks for meeting", "final": false}
//   {"text": "hi thanks for meeting with me today", "final": true}
// plus status lines:
//   {"event": "lm_ready"} / {"event": "lm_unavailable", "reason": "..."}
//   {"other": true} / {"other": false, "secs": 4.2}   (with --system-audio)
//
// FILE mode (--file <audio>): transcribes a finished recording and prints one
//   {"file_text": "...", "words": 2412, "speaking_secs": 1040.5}
// then exits.
//
// Options:
//   --script <path>    Build a custom language model from this script (macOS 14+)
//                      so its wording and drug names are favoured.
//   --record <path>    Also write the microphone to a 16 kHz mono CAF file.
//   --system-audio     Watch system audio (the call) and report when the other
//                      party is speaking (ScreenCaptureKit, macOS 13+).
//   --file <path>      File mode (see above).
//
// Errors go to stderr as JSON: {"error": "..."}.
// Stops cleanly on SIGTERM/SIGINT (the recording is closed properly).

import AVFoundation
import Foundation
import ScreenCaptureKit
import Speech

// ── Arguments ──

var scriptPath: String?
var recordPath: String?
var filePath: String?
var watchSystemAudio = false
do {
    var args = CommandLine.arguments.dropFirst().makeIterator()
    while let a = args.next() {
        switch a {
        case "--script": scriptPath = args.next()
        case "--record": recordPath = args.next()
        case "--file": filePath = args.next()
        case "--system-audio": watchSystemAudio = true
        default: break
        }
    }
}

// ── Output helpers ──

let outLock = NSLock()
func emit(_ obj: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: obj),
          let line = String(data: data, encoding: .utf8) else { return }
    outLock.lock()
    print(line)
    fflush(stdout)
    outLock.unlock()
}
func fail(_ msg: String) {
    let obj = ["error": msg]
    if let data = try? JSONSerialization.data(withJSONObject: obj) {
        FileHandle.standardError.write(data + "\n".data(using: .utf8)!)
    }
}

// ── Authorization ──

let authSem = DispatchSemaphore(value: 0)
var authStatus: SFSpeechRecognizerAuthorizationStatus = .notDetermined
SFSpeechRecognizer.requestAuthorization { status in
    authStatus = status
    authSem.signal()
}
if authSem.wait(timeout: .now() + 30) == .timedOut || authStatus != .authorized {
    let msg: String
    switch authStatus {
    case .denied: msg = "denied"
    case .restricted: msg = "restricted"
    case .notDetermined: msg = "not_determined"
    default: msg = "unknown"
    }
    fail("speech_auth_\(msg)")
    exit(1)
}

guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en-US")),
      recognizer.isAvailable else {
    fail("recognizer_unavailable")
    exit(1)
}

// ── Custom language model from the script ──

/// Build (or reuse) a language model biased toward the script's own sentences.
/// Every sentence is inserted as a phrase, so the recognizer expects exactly
/// this wording, including drug names it would otherwise mishear.
func prepareLanguageModel(scriptPath: String) -> Any? {
    guard #available(macOS 14, *) else {
        emit(["event": "lm_unavailable", "reason": "needs macOS 14"])
        return nil
    }
    guard let text = try? String(contentsOfFile: scriptPath, encoding: .utf8) else {
        emit(["event": "lm_unavailable", "reason": "script unreadable"])
        return nil
    }
    // Sentences: split on line breaks and sentence punctuation; drop markup.
    var phrases: [String] = []
    for raw in text.components(separatedBy: CharacterSet(charactersIn: "\n.!?")) {
        var s = raw.trimmingCharacters(in: .whitespaces)
        while let f = s.first, "#>-*".contains(f) {
            s.removeFirst()
            s = s.trimmingCharacters(in: .whitespaces)
        }
        if s.hasPrefix("PAUSE:") || s.hasPrefix("BRANCH:") {
            s = String(s.drop(while: { $0 != ":" }).dropFirst()).trimmingCharacters(in: .whitespaces)
        }
        if s.contains(":") && s.split(separator: " ").count <= 2 { continue } // frontmatter keys
        if s.split(separator: " ").count >= 2 { phrases.append(s) }
    }
    if phrases.isEmpty {
        emit(["event": "lm_unavailable", "reason": "no phrases"])
        return nil
    }
    let dir = URL(fileURLWithPath: scriptPath).deletingLastPathComponent()
    let asset = dir.appendingPathComponent("script-lm.bin")
    let lmURL = dir.appendingPathComponent("script-lm.model")
    let vocabURL = dir.appendingPathComponent("script-lm.vocab")
    let data = SFCustomLanguageModelData(
        locale: Locale(identifier: "en-US"),
        identifier: "com.rxvip.prompter.script",
        version: "1.0"
    )
    for p in phrases {
        data.insert(phraseCount: SFCustomLanguageModelData.PhraseCount(phrase: p, count: 20))
    }
    let sem = DispatchSemaphore(value: 0)
    var exportError: Error?
    Task {
        do { try await data.export(to: asset) } catch { exportError = error }
        sem.signal()
    }
    sem.wait()
    if let e = exportError {
        emit(["event": "lm_unavailable", "reason": "export: \(e.localizedDescription)"])
        return nil
    }
    let config = SFSpeechLanguageModel.Configuration(languageModel: lmURL, vocabulary: vocabURL)
    var prepError: Error?
    SFSpeechLanguageModel.prepareCustomLanguageModel(for: asset, configuration: config) { err in
        prepError = err
        sem.signal()
    }
    if sem.wait(timeout: .now() + 120) == .timedOut {
        emit(["event": "lm_unavailable", "reason": "prepare timed out"])
        return nil
    }
    if let e = prepError {
        emit(["event": "lm_unavailable", "reason": "prepare: \(e.localizedDescription)"])
        return nil
    }
    emit(["event": "lm_ready", "phrases": phrases.count])
    return config
}

let lmConfig: Any? = scriptPath.flatMap { prepareLanguageModel(scriptPath: $0) }

func configure(_ request: SFSpeechRecognitionRequest) {
    request.requiresOnDeviceRecognition = true // local only, no network
    if #available(macOS 14, *), let cfg = lmConfig as? SFSpeechLanguageModel.Configuration {
        request.customizedLanguageModel = cfg
    }
}

// ── FILE mode: transcribe a finished recording ──

/// Transcribe in ~50 s chunks (each its own request) so a long consult never
/// hits a per-request limit; word timings give the time actually spent talking.
func transcribeFile(_ path: String) -> Never {
    let url = URL(fileURLWithPath: path)
    guard let file = try? AVAudioFile(forReading: url) else {
        fail("file_unreadable")
        exit(1)
    }
    let format = file.processingFormat
    let chunkFrames = AVAudioFrameCount(format.sampleRate * 50)
    var pieces: [String] = []
    var spans: [(Double, Double)] = [] // absolute word start/end seconds
    var chunkStart = 0.0
    while file.framePosition < file.length {
        let request = SFSpeechAudioBufferRecognitionRequest()
        configure(request)
        request.shouldReportPartialResults = false
        var framesLeft = chunkFrames
        while framesLeft > 0 && file.framePosition < file.length {
            let n = min(AVAudioFrameCount(4096), framesLeft)
            guard let buf = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: n) else { break }
            do { try file.read(into: buf, frameCount: n) } catch { break }
            if buf.frameLength == 0 { break }
            request.append(buf)
            framesLeft -= buf.frameLength
        }
        request.endAudio()
        let sem = DispatchSemaphore(value: 0)
        var best: SFTranscription?
        let task = recognizer.recognitionTask(with: request) { result, error in
            if let r = result {
                best = r.bestTranscription
                if r.isFinal { sem.signal() }
            } else if error != nil {
                sem.signal()
            }
        }
        if sem.wait(timeout: .now() + 180) == .timedOut { task.cancel() }
        if let t = best {
            pieces.append(t.formattedString)
            for seg in t.segments {
                spans.append((chunkStart + seg.timestamp, chunkStart + seg.timestamp + seg.duration))
            }
        }
        chunkStart += Double(chunkFrames) / format.sampleRate
    }
    // Speaking time: word spans merged across gaps shorter than one second
    // (natural phrase pauses count as speaking; longer silences don't).
    var speaking = 0.0
    var phrase: (Double, Double)?
    for s in spans.sorted(by: { $0.0 < $1.0 }) {
        if let p = phrase, s.0 - p.1 < 1.0 {
            phrase = (p.0, max(p.1, s.1))
        } else {
            if let p = phrase { speaking += p.1 - p.0 }
            phrase = s
        }
    }
    if let p = phrase { speaking += p.1 - p.0 }
    emit(["file_text": pieces.joined(separator: " "), "words": spans.count, "speaking_secs": speaking])
    exit(0)
}

if let f = filePath { transcribeFile(f) }

// ── LIVE mode ──

let audioEngine = AVAudioEngine()
let inputNode = audioEngine.inputNode
let request = SFSpeechAudioBufferRecognitionRequest()
request.shouldReportPartialResults = true
configure(request)

var lastOutput = ""
let task = recognizer.recognitionTask(with: request) { result, error in
    if let result = result {
        let text = result.bestTranscription.formattedString
        if text != lastOutput {
            lastOutput = text
            emit(["text": text, "final": result.isFinal])
        }
        if result.isFinal { lastOutput = "" }
    }
    if let error = error as NSError? {
        if error.code == 1110 { return } // no speech detected
        fail(error.localizedDescription)
    }
}

// Optional recording: 16 kHz mono 16-bit PCM in CAF (small, and CAF stays
// readable even if the process dies mid-write).
var recorder: AVAudioFile?
var converter: AVAudioConverter?
let inputFormat = inputNode.outputFormat(forBus: 0)
let recordFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16_000, channels: 1, interleaved: false)!
if let path = recordPath {
    let url = URL(fileURLWithPath: path)
    let settings: [String: Any] = [
        AVFormatIDKey: kAudioFormatLinearPCM,
        AVSampleRateKey: 16_000,
        AVNumberOfChannelsKey: 1,
        AVLinearPCMBitDepthKey: 16,
        AVLinearPCMIsFloatKey: false,
        AVLinearPCMIsBigEndianKey: false,
    ]
    do {
        recorder = try AVAudioFile(forWriting: url, settings: settings, commonFormat: .pcmFormatFloat32, interleaved: false)
        try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: path)
        converter = AVAudioConverter(from: inputFormat, to: recordFormat)
    } catch {
        fail("record_open: \(error.localizedDescription)")
    }
}
let recordQueue = DispatchQueue(label: "prompter.record")

inputNode.installTap(onBus: 0, bufferSize: 1024, format: inputFormat) { buffer, _ in
    request.append(buffer)
    guard let conv = converter else { return }
    let ratio = recordFormat.sampleRate / buffer.format.sampleRate
    let cap = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 16
    guard let out = AVAudioPCMBuffer(pcmFormat: recordFormat, frameCapacity: cap) else { return }
    var fed = false
    var err: NSError?
    conv.convert(to: out, error: &err) { _, status in
        if fed {
            status.pointee = .noDataNow
            return nil
        }
        fed = true
        status.pointee = .haveData
        return buffer
    }
    if err == nil && out.frameLength > 0 {
        recordQueue.async { try? recorder?.write(from: out) }
    }
}

do {
    audioEngine.prepare()
    try audioEngine.start()
    emit(["event": "listening"])
} catch {
    fail("audio_engine: \(error.localizedDescription)")
    exit(1)
}

// ── Other-party detection from system audio (the call) ──

/// Energy detector over system audio with hysteresis: "speaking" after 0.2 s
/// above the floor, "done" after 0.7 s below it (a short end-of-turn wait).
final class SystemAudioWatcher: NSObject, SCStreamOutput, SCStreamDelegate {
    private var stream: SCStream?
    private var speaking = false
    private var aboveSince: Double?
    private var belowSince: Double?
    private var turnStart = 0.0
    private let threshold: Float = 0.012

    func start() {
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: false) { content, error in
            guard let display = content?.displays.first else {
                fail("screen_capture_denied")
                return
            }
            let filter = SCContentFilter(display: display, excludingWindows: [])
            let cfg = SCStreamConfiguration()
            cfg.capturesAudio = true
            cfg.excludesCurrentProcessAudio = true
            cfg.sampleRate = 16_000
            cfg.channelCount = 1
            cfg.width = 2
            cfg.height = 2
            cfg.minimumFrameInterval = CMTime(value: 1, timescale: 1)
            let s = SCStream(filter: filter, configuration: cfg, delegate: self)
            do {
                try s.addStreamOutput(self, type: .audio, sampleHandlerQueue: DispatchQueue(label: "prompter.sysaudio"))
                s.startCapture { err in
                    if let err = err { fail("system_audio: \(err.localizedDescription)") }
                    else { emit(["event": "system_audio_ready"]) }
                }
                self.stream = s
            } catch {
                fail("system_audio: \(error.localizedDescription)")
            }
        }
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio else { return }
        let now = CMTimeGetSeconds(CMSampleBufferGetPresentationTimeStamp(sb))
        var abl = AudioBufferList()
        var block: CMBlockBuffer?
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sb, bufferListSizeNeededOut: nil, bufferListOut: &abl,
            bufferListSize: MemoryLayout<AudioBufferList>.size, blockBufferAllocator: nil,
            blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: &block)
        guard status == noErr, let data = abl.mBuffers.mData else { return }
        let count = Int(abl.mBuffers.mDataByteSize) / MemoryLayout<Float>.size
        if count == 0 { return }
        let samples = data.bindMemory(to: Float.self, capacity: count)
        var sum: Float = 0
        for i in 0..<count { sum += samples[i] * samples[i] }
        let rms = (sum / Float(count)).squareRoot()

        if rms >= threshold {
            belowSince = nil
            if aboveSince == nil { aboveSince = now }
            if !speaking, let a = aboveSince, now - a >= 0.2 {
                speaking = true
                turnStart = a
                emit(["other": true])
            }
        } else {
            aboveSince = nil
            if belowSince == nil { belowSince = now }
            if speaking, let b = belowSince, now - b >= 0.7 {
                speaking = false
                emit(["other": false, "secs": max(0, b - turnStart)])
            }
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        fail("system_audio_stopped: \(error.localizedDescription)")
    }

    func stop() { stream?.stopCapture(completionHandler: nil) }
}

let watcher: SystemAudioWatcher? = watchSystemAudio ? SystemAudioWatcher() : nil
watcher?.start()

// ── Clean shutdown ──

func shutdown() -> Never {
    audioEngine.stop()
    inputNode.removeTap(onBus: 0)
    request.endAudio()
    watcher?.stop()
    recordQueue.sync { recorder = nil } // closes the file
    exit(0)
}
signal(SIGTERM, SIG_IGN)
signal(SIGINT, SIG_IGN)
let termSource = DispatchSource.makeSignalSource(signal: SIGTERM, queue: .main)
termSource.setEventHandler { shutdown() }
termSource.resume()
let intSource = DispatchSource.makeSignalSource(signal: SIGINT, queue: .main)
intSource.setEventHandler { shutdown() }
intSource.resume()

_ = task
RunLoop.main.run()
