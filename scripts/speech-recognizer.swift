// Real-time speech recognition using Apple's Speech framework.
// Streams recognized text to stdout as JSON lines:
//   {"text": "hi thanks for meeting", "final": false}
//   {"text": "hi thanks for meeting with me today", "final": true}
//
// Usage: swift speech-recognizer.swift
// Runs until killed (SIGTERM/SIGINT).

import Speech
import Foundation

// Request authorization
SFSpeechRecognizer.requestAuthorization { status in
    guard status == .authorized else {
        let msg: String
        switch status {
        case .denied: msg = "denied"
        case .restricted: msg = "restricted"
        case .notDetermined: msg = "not_determined"
        default: msg = "unknown"
        }
        FileHandle.standardError.write("{\"error\": \"speech_auth_\(msg)\"}\n".data(using: .utf8)!)
        exit(1)
    }
}

// Wait for auth
RunLoop.current.run(until: Date(timeIntervalSinceNow: 1))

guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: "en-US")),
      recognizer.isAvailable else {
    FileHandle.standardError.write("{\"error\": \"recognizer_unavailable\"}\n".data(using: .utf8)!)
    exit(1)
}

let audioEngine = AVAudioEngine()
let inputNode = audioEngine.inputNode
let request = SFSpeechAudioBufferRecognitionRequest()
request.shouldReportPartialResults = true
request.requiresOnDeviceRecognition = true  // Local, no network needed

// Track what we've already output to avoid duplicates
var lastOutput = ""

let task = recognizer.recognitionTask(with: request) { result, error in
    if let result = result {
        let text = result.bestTranscription.formattedString
        let isFinal = result.isFinal

        // Only output if text changed
        if text != lastOutput {
            lastOutput = text
            // JSON-escape the text
            let escaped = text
                .replacingOccurrences(of: "\\", with: "\\\\")
                .replacingOccurrences(of: "\"", with: "\\\"")
            let json = "{\"text\": \"\(escaped)\", \"final\": \(isFinal)}"
            print(json)
            fflush(stdout)
        }

        if isFinal {
            lastOutput = ""
        }
    }

    if let error = error {
        let nsError = error as NSError
        // Error 1110 = no speech detected, just restart
        if nsError.code == 1110 {
            return
        }
        FileHandle.standardError.write("{\"error\": \"\(error.localizedDescription)\"}\n".data(using: .utf8)!)
    }
}

// Install audio tap
let format = inputNode.outputFormat(forBus: 0)
inputNode.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
    request.append(buffer)
}

// Start audio engine
do {
    audioEngine.prepare()
    try audioEngine.start()
    FileHandle.standardError.write("{\"status\": \"listening\"}\n".data(using: .utf8)!)
} catch {
    FileHandle.standardError.write("{\"error\": \"audio_engine: \(error.localizedDescription)\"}\n".data(using: .utf8)!)
    exit(1)
}

// Handle SIGTERM/SIGINT gracefully
signal(SIGTERM) { _ in
    audioEngine.stop()
    inputNode.removeTap(onBus: 0)
    request.endAudio()
    exit(0)
}
signal(SIGINT) { _ in
    audioEngine.stop()
    inputNode.removeTap(onBus: 0)
    request.endAudio()
    exit(0)
}

// Run forever
RunLoop.current.run()
