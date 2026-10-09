import Foundation
@testable import JoseonCore
import JoseonHeadphones

// One line per value list: a name, then the numbers. Floats print in their shortest form
// that reads back to the same bits.
var lines: [String] = []
func put(_ name: String, _ values: [Float]) { lines.append(name + " " + values.map { "\($0)" }.joined(separator: " ")) }
func put(_ name: String, _ text: String) { lines.append(name + " " + text) }

func peakText(_ p: PeakReading) -> String { "\(p.frequencyHz) \(p.levelDB) \(p.cents) [\(p.noteName)]" }

/// Feeds `left`/`right` in blocks and reads after each block, as the engine does.
func runSpectrum(name: String, left: [Float], right: [Float], rate: Double, block: Int, settings: SpectrumSettings,
                 checkpoints: [Int], resetAt: Int? = nil) {
    let analyzer = SpectrumAnalyzer(settings: settings)
    var position = 0
    var blockIndex = 0
    var marks = checkpoints
    while position < left.count {
        let n = min(block, left.count - position)
        left.withUnsafeBufferPointer { l in
            right.withUnsafeBufferPointer { r in
                analyzer.process(left: l.baseAddress! + position, right: r.baseAddress! + position, count: n, sampleRate: rate)
            }
        }
        position += n
        blockIndex += 1
        let out = analyzer.read()
        if blockIndex == resetAt { analyzer.reset() }
        guard marks.first == blockIndex else { continue }
        marks.removeFirst()
        let tag = "\(name)@\(blockIndex)"
        put("\(tag) left", out.spectrum.left)
        put("\(tag) right", out.spectrum.right)
        put("\(tag) mid", out.spectrum.mid)
        put("\(tag) side", out.spectrum.side)
        put("\(tag) peakHold", out.spectrum.peakHold)
        put("\(tag) average", out.spectrum.average)
        for (i, layer) in out.spectrum.midLayers.enumerated() {
            put("\(tag) layer\(i)", layer.levelsDB)
            put("\(tag) latency\(i)", [layer.latencySeconds])
        }
        put("\(tag) bands", out.bands.values)
        put("\(tag) peak", peakText(out.peak))
        put("\(tag) topPeaks", analyzer.topPeaks.map(peakText).joined(separator: " | "))
        put("\(tag) lowestStrongHz", [analyzer.lowestStrongHz])
        if let third = analyzer.thirdOctave {
            put("\(tag) thirdLeft", third.left)
            put("\(tag) thirdRight", third.right)
        } else {
            put("\(tag) thirdLeft", "nil")
        }
    }
}

// 1. The demo signal at 48 kHz, 800-frame blocks (60 per second), 8 seconds.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 48_000 * 8)
    runSpectrum(name: "demo48", left: d.left, right: d.right, rate: 48_000, block: 800, settings: SpectrumSettings(),
                checkpoints: [30, 120, 300, 480])
}

// 2. Tones over pink noise at 44.1 kHz, display tilt, odd block size, a reset in the middle.
do {
    let rate = 44_100.0, seconds = 6.0
    let n = Int(rate * seconds)
    let noise = TestSignals.pinkNoise(amplitude: 0.05, count: n, seed: 7)
    let noiseR = TestSignals.pinkNoise(amplitude: 0.05, count: n, seed: 8)
    var l = [Float](repeating: 0, count: n), r = l
    let tones: [(Double, Float, Float, Float)] = [(31.5, 0.25, 1, 1), (220, 0.1, 1, 1), (1_000, 0.2, 1, 0.5), (3_520, 0.02, 0, 1), (12_000, 0.05, 1, -1)]
    for (hz, amp, gl, gr) in tones {
        let s = TestSignals.sine(hz: hz, amplitude: amp, sampleRate: rate, seconds: seconds)
        for i in 0..<n { l[i] += s[i] * gl; r[i] += s[i] * gr }
    }
    for i in 0..<n { l[i] += noise[i]; r[i] += noiseR[i] }
    var settings = SpectrumSettings()
    settings.tiltDBPerOctave = 4.5
    settings.displayBins = 600
    settings.minHz = 20
    settings.maxHz = 20_000
    runSpectrum(name: "tones44", left: l, right: r, rate: rate, block: 441, settings: settings,
                checkpoints: [100, 300, 600], resetAt: 250)
}

// 3. The demo signal at 96 kHz, big blocks (a late caller), then silence.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 96_000 * 5, sampleRate: 96_000)
    let quiet = [Float](repeating: 0, count: 96_000 * 2)
    runSpectrum(name: "demo96", left: d.left + quiet, right: d.right + quiet, rate: 96_000, block: 4_096, settings: SpectrumSettings(),
                checkpoints: [60, 117, 130, 164])
}

/// The `crates` directory of the JoseonPC repository.
let directory = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "crates"

/// Writes the lines collected so far to the golden file `name` of `crate` and starts a new file.
func write(_ name: String, crate: String = "joseon-core") {
    let url = URL(fileURLWithPath: directory).appendingPathComponent("\(crate)/tests/golden/\(name)")
    try! (lines.joined(separator: "\n") + "\n").write(to: url, atomically: true, encoding: .utf8)
    print("wrote \(lines.count) lines to \(url.path)")
    lines.removeAll()
}

write("spectrum.txt")

// MARK: - Loudness

/// Feeds the parts one after the other (a part can have its own sample rate) and writes every
/// field of the reading at the checkpoints. Block counts run over all parts.
func runLoudness(name: String, parts: [(left: [Float], right: [Float], rate: Double)], block: Int,
                 checkpoints: [Int], resetAt: Int? = nil) {
    let meter = LoudnessMeter()
    var blockIndex = 0
    var marks = checkpoints
    for part in parts {
        var position = 0
        while position < part.left.count {
            let n = min(block, part.left.count - position)
            part.left.withUnsafeBufferPointer { l in
                part.right.withUnsafeBufferPointer { r in
                    meter.process(left: l.baseAddress! + position, right: r.baseAddress! + position, count: n, sampleRate: part.rate)
                }
            }
            position += n
            blockIndex += 1
            if blockIndex == resetAt { meter.reset() }
            guard marks.first == blockIndex else { continue }
            marks.removeFirst()
            let r = meter.read()
            put("\(name)@\(blockIndex) levels", [
                r.momentaryLUFS, r.shortTermLUFS, r.integratedLUFS, r.momentaryMaxLUFS, r.shortTermMaxLUFS, r.loudnessRangeLU,
                r.truePeakLeftDBTP, r.truePeakRightDBTP, r.truePeakMaxDBTP, r.rmsLeftDB, r.rmsRightDB, r.plrDB, r.psrDB,
            ])
            put("\(name)@\(blockIndex) counts", "\(r.clipCount) \(r.measuredSeconds) \(r.isIntegratedValid)")
        }
    }
}

// 1. The demo signal at 48 kHz for 40 seconds.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 48_000 * 40)
    runLoudness(name: "demo48", parts: [(d.left, d.right, 48_000)], block: 800, checkpoints: [3, 50, 300, 1_200, 1_777, 2_400])
}

// 2. A tone that steps in level over pink noise at 44.1 kHz, with a hard-clipped stretch, a
//    quiet tail, odd blocks and a reset.
do {
    let rate = 44_100.0
    func tone(_ hz: Double, _ db: Double, _ seconds: Double) -> [Float] {
        TestSignals.sine(hz: hz, amplitude: Float(pow(10, db / 20)), sampleRate: rate, seconds: seconds)
    }
    var l = tone(1_000, -36, 6) + tone(1_000, -23, 12) + tone(1_000, -30, 8)
    var r = tone(997, -30, 6) + tone(997, -20, 12) + tone(997, -36, 8)
    let noise = TestSignals.pinkNoise(amplitude: 0.01, count: l.count, seed: 3)
    for i in 0..<l.count { l[i] += noise[i]; r[i] -= noise[i] }
    let loud = TestSignals.sine(hz: 440, amplitude: 2, sampleRate: rate, seconds: 2).map { max(-1, min(1, $0)) }
    l += loud; r += loud.map { $0 * 0.5 }
    let tail = TestSignals.pinkNoise(amplitude: 0.0001, count: Int(rate * 6), seed: 4)
    l += tail; r += tail
    runLoudness(name: "steps44", parts: [(l, r, rate)], block: 441, checkpoints: [100, 600, 1_800, 2_700, 2_810, 3_050, 3_400],
                resetAt: 2_750)
}

// 3. A device switch: 48 kHz, then 96 kHz, then 192 kHz (2x oversampling), in big blocks.
do {
    let a = TestSignals.demoBlock(startSample: 0, count: 48_000 * 5, sampleRate: 48_000)
    let b = TestSignals.demoBlock(startSample: 0, count: 96_000 * 5, sampleRate: 96_000)
    let c = TestSignals.sine(hz: 40_000, amplitude: 0.7, sampleRate: 192_000, seconds: 3, phase: 0.4)
    let silence = [Float](repeating: 0, count: 192_000 * 3)
    runLoudness(name: "switch", parts: [(a.left, a.right, 48_000), (b.left, b.right, 96_000), (c, c.map { -$0 }, 192_000),
                                        (silence, silence, 192_000)],
                block: 4_096, checkpoints: [40, 59, 100, 176, 200, 300, 350, 458])
}

write("loudness.txt")

// MARK: - Stereo

func fnv(_ values: [Float]) -> String {
    var h: UInt64 = 0xcbf29ce484222325
    for v in values { h = (h ^ UInt64(v.bitPattern)) &* 0x100000001b3 }
    return String(h, radix: 16)
}

func runStereo(name: String, parts: [(left: [Float], right: [Float], rate: Double)], block: Int,
               checkpoints: [Int], resetAt: Int? = nil, scopePoints: Int = 2048) {
    let analyzer = StereoAnalyzer()
    analyzer.scopePointCount = scopePoints
    var blockIndex = 0
    var marks = checkpoints
    for part in parts {
        var position = 0
        while position < part.left.count {
            let n = min(block, part.left.count - position)
            part.left.withUnsafeBufferPointer { l in
                part.right.withUnsafeBufferPointer { r in
                    analyzer.process(left: l.baseAddress! + position, right: r.baseAddress! + position, count: n, sampleRate: part.rate)
                }
            }
            position += n
            blockIndex += 1
            if blockIndex == resetAt { analyzer.reset() }
            guard marks.first == blockIndex else { continue }
            marks.removeFirst()
            let r = analyzer.read()
            let tag = "\(name)@\(blockIndex)"
            put("\(tag) broadband", [r.correlation, r.balance, r.width])
            put("\(tag) bandCorrelation", r.bandCorrelation)
            put("\(tag) bandBalance", r.bandBalance)
            put("\(tag) bandActive", r.bandActive.map { $0 ? "1" : "0" }.joined(separator: " "))
            put("\(tag) scope", "\(r.scopePoints.count) \(fnv(r.scopePoints.flatMap { [$0.x, $0.y] }))")
        }
    }
}

// 1. The demo signal at 48 kHz.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 48_000 * 12)
    runStereo(name: "demo48", parts: [(d.left, d.right, 48_000)], block: 800, checkpoints: [1, 2, 30, 120, 400, 720])
}

// 2. 44.1 kHz: mono bass, an anti-phase tone, a right-only tone, two noises, odd blocks, a reset, then near silence.
do {
    let rate = 44_100.0, seconds = 6.0
    let n = Int(rate * seconds)
    func tone(_ hz: Double, _ a: Float, _ phase: Double = 0) -> [Float] { TestSignals.sine(hz: hz, amplitude: a, sampleRate: rate, seconds: seconds, phase: phase) }
    let bass = tone(90, 0.3), anti = tone(5_000, 0.2), rightOnly = tone(1_000, 0.25), wideL = tone(3_000, 0.1), wideR = tone(3_000, 0.1, .pi / 3)
    let nl = TestSignals.pinkNoise(amplitude: 0.02, count: n, seed: 21), nr = TestSignals.pinkNoise(amplitude: 0.02, count: n, seed: 22)
    var l = [Float](repeating: 0, count: n), r = l
    for i in 0..<n {
        l[i] = bass[i] + anti[i] + wideL[i] + nl[i]
        r[i] = bass[i] - anti[i] + rightOnly[i] + wideR[i] + nr[i]
    }
    let quiet = TestSignals.whiteNoise(amplitude: 0.000001, count: Int(rate * 2), seed: 23)
    runStereo(name: "mix44", parts: [(l + quiet, r + quiet.map { -$0 }, rate)], block: 441, checkpoints: [10, 150, 400, 590, 640, 780],
              resetAt: 420, scopePoints: 500)
}

// 3. A device switch from 48 kHz to 96 kHz in big blocks, with a full-scale stretch.
do {
    let a = TestSignals.demoBlock(startSample: 0, count: 48_000 * 3, sampleRate: 48_000)
    let b = TestSignals.demoBlock(startSample: 0, count: 96_000 * 3, sampleRate: 96_000)
    let hotL = TestSignals.whiteNoise(amplitude: 1, count: 96_000, seed: 31), hotR = TestSignals.whiteNoise(amplitude: 1, count: 96_000, seed: 32)
    runStereo(name: "switch", parts: [(a.left, a.right, 48_000), (b.left, b.right, 96_000), (hotL, hotR, 96_000)], block: 4_096,
              checkpoints: [5, 36, 37, 80, 106, 130], scopePoints: 8_192)
}

write("stereo.txt")

// MARK: - Engine and session record

/// Runs the whole engine offline, one `processNow` per block, and writes what the session record
/// holds at the end. Dates are left out: they come from the wall clock.
func runEngine(name: String, left: [Float], right: [Float], rate: Double, block: Int, resetAt: [Int], sleeps: Bool) {
    let engine = AnalysisEngine()
    engine.sleepsInProcessNow = sleeps
    var position = 0
    var blockIndex = 0
    var silentFrames = 0, asleepBlocks = 0
    var last = engine.latestFrame
    while position < left.count {
        let n = min(block, left.count - position)
        last = engine.processNow(left: Array(left[position..<(position + n)]), right: Array(right[position..<(position + n)]), count: n, sampleRate: rate)
        position += n
        blockIndex += 1
        if last.isSilent { silentFrames += 1 }
        if engine.isAsleep { asleepBlocks += 1 }
        if resetAt.contains(blockIndex) { engine.resetMeasurement() }
    }
    let snapshot = engine.session.snapshot()
    put("\(name) counts", "\(blockIndex) \(silentFrames) \(asleepBlocks) \(snapshot.samples.count) \(snapshot.events.count) \(last.isSilent)")
    put("\(name) lowestStrongHz", [last.lowestStrongHz])
    for (i, s) in snapshot.samples.enumerated() {
        put("\(name) sample\(i)", [Float(s.time), s.momentaryMaxLUFS, s.shortTermLUFS, s.truePeakDBTP, s.correlation] + s.bands + [s.isSilent ? 1 : 0])
    }
    for (i, e) in snapshot.events.enumerated() {
        put("\(name) event\(i)", "\(e.id) \(e.kind.rawValue) \(Float(e.time)) \(e.value) [\(e.label)]")
    }
}

// 1. Twelve seconds of the demo signal with a measurement reset on the way.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 48_000 * 12)
    runEngine(name: "demo", left: d.left, right: d.right, rate: 48_000, block: 800, resetAt: [400], sleeps: false)
}

// 2. A tone, a hard-clipped stretch, a long silence that puts the engine to sleep, then the tone again.
do {
    let rate = 48_000.0
    let tone = TestSignals.sine(hz: 440, amplitude: 0.5, sampleRate: rate, seconds: 3)
    let hot = TestSignals.sine(hz: 440, amplitude: 2, sampleRate: rate, seconds: 1.5).map { max(-1, min(1, $0)) }
    let silence = [Float](repeating: 0, count: Int(rate * 14))
    let l = tone + hot + silence + tone
    let r = tone.map { $0 * 0.5 } + hot + silence + tone
    runEngine(name: "sleep", left: l, right: r, rate: rate, block: 4_800, resetAt: [], sleeps: true)
}

write("engine.txt")

// MARK: - Headphone model and stress flags

/// Text fields go on their own lines: they hold spaces.
func putFlags(_ tag: String, _ flags: [StressFlag]) {
    put("\(tag) flagCount", "\(flags.count)")
    for (i, f) in flags.enumerated() {
        let range = f.frequencyRangeHz.map { "\($0.lowerBound) \($0.upperBound)" } ?? "none"
        put("\(tag) flag\(i)", "\(f.id) \(f.severity.rawValue) \(range)")
        put("\(tag) flag\(i).title", f.title)
        put("\(tag) flag\(i).detail", f.detail)
        put("\(tag) flag\(i).plotLabel", f.plotLabel)
    }
}

/// The engine with a headphone model, one `processNow` per block. The clock of the model is the
/// audio clock, so the flag hysteresis does not depend on how fast the machine is.
func runHeadphone(name: String, curve: HeadphoneCurve, target: HeadphoneCurve?, left: [Float], right: [Float],
                  rate: Double, block: Int, checkpoints: [Int], resetAt: Int? = nil) {
    var audioSeconds = 0.0
    let model = HeadphoneModel(curve: curve, target: target, thresholds: StressThresholds(), now: { audioSeconds })
    let engine = AnalysisEngine()
    engine.headphoneModel = model
    var position = 0, blockIndex = 0
    var marks = checkpoints
    while position < left.count {
        let n = min(block, left.count - position)
        audioSeconds = Double(position + n) / rate
        let frame = engine.processNow(left: Array(left[position..<(position + n)]), right: Array(right[position..<(position + n)]),
                                      count: n, sampleRate: rate)
        position += n
        blockIndex += 1
        if blockIndex == resetAt { engine.resetMeasurement() }
        guard marks.first == blockIndex, let h = frame.headphone else { continue }
        marks.removeFirst()
        let tag = "\(name)@\(blockIndex)"
        put("\(tag) model", "\(h.hasTarget) [\(h.modelName)]")
        put("\(tag) response", h.responseDB)
        put("\(tag) target", h.targetDB)
        put("\(tag) predicted", h.predictedAtEarDB)
        putFlags(tag, h.stressFlags)
        let onsets = model.flagOnsets()
        put("\(tag) onsets", onsets.keys.sorted().map { "\($0)=\(Float(onsets[$0]!))" }.joined(separator: " "))
    }
}

// Static numbers of every embedded curve: its reference level, and what the stress context says about it.
do {
    let harman = EmbeddedCurves.harmanOverEar2018
    for curve in EmbeddedCurves.headphones + EmbeddedCurves.targets {
        let model = HeadphoneModel(curve: curve, target: harman)
        let c = model.stressContext
        let peak = c.maxExcessOverTarget(from: 4_000, to: 10_000)
        put("curve [\(curve.name)]", [curve.referenceLevelDB, c.meanSubBassResponseDB ?? -999, c.meanShortfallDB(from: 20, to: 40) ?? -999,
                                      peak?.excessDB ?? -999, peak?.hz ?? -999, curve.rangeHz.low, curve.rangeHz.high])
    }
    // The interpolator on a grid that starts below and ends above the measured range.
    let grid: [Float] = (0..<60).map { 5 * pow(2, Float($0) / 4.5) }
    put("interpolated grid", grid)
    put("interpolated hd600", CurveInterpolator(curve: EmbeddedCurves.hd600.normalizedTo1kHz()).levels(atHz: grid))
    put("interpolated single", grid.map { CurveInterpolator(curve: EmbeddedCurves.lcdX).level(atHz: $0) })
}

// 1. The demo signal on the Susvara Unveiled against Harman: strong sub-bass, and nothing else.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 48_000 * 20)
    runHeadphone(name: "susvara", curve: EmbeddedCurves.susvaraUnveiled, target: EmbeddedCurves.harmanOverEar2018,
                 left: d.left, right: d.right, rate: 48_000, block: 800, checkpoints: [60, 170, 240, 600, 1_200])
}

/// A loud, clipped, bright signal with a 30 Hz rumble. After `quietAfter` seconds it is nearly silent.
func loudSignal(seconds: Double, quietAfter: Double) -> (left: [Float], right: [Float]) {
    let rate = 48_000.0
    let n = Int(rate * seconds)
    let noise = TestSignals.pinkNoise(amplitude: 0.9, count: n, seed: 41)
    let hiss = TestSignals.whiteNoise(amplitude: 0.3, count: n, seed: 42)
    let rumble = TestSignals.sine(hz: 30, amplitude: 0.6, sampleRate: rate, seconds: seconds)
    var l = [Float](repeating: 0, count: n), r = l
    for i in 0..<n {
        let quiet: Float = (i > Int(rate * quietAfter)) ? 0.001 : 1
        l[i] = max(-1, min(1, (noise[i] + hiss[i] + rumble[i]) * 1.6)) * quiet
        r[i] = max(-1, min(1, (noise[i] - hiss[i] + rumble[i]) * 1.6)) * quiet
    }
    return (l, r)
}

// 2. The loud signal on an in-ear curve against its target: sub-bass load, overs, clips, then a dense master.
//    A measurement reset near the end, then near silence.
do {
    let s = loudSignal(seconds: 40, quietAfter: 33)
    runHeadphone(name: "loud", curve: EmbeddedCurves.airPodsPro2, target: EmbeddedCurves.harmanInEar2019,
                 left: s.left, right: s.right, rate: 48_000, block: 800, checkpoints: [30, 200, 400, 1_900, 2_000, 2_050, 2_390], resetAt: 2_040)
}

// 4. The loud signal on a headphone that is short of deep bass against the target (under-delivery),
//    and on one with a treble peak over the target (hot spot). The signal then goes quiet: both flags
//    hold for a time, then clear.
do {
    let s = loudSignal(seconds: 20, quietAfter: 8)
    runHeadphone(name: "short", curve: EmbeddedCurves.hd600, target: EmbeddedCurves.harmanOverEar2018,
                 left: s.left, right: s.right, rate: 48_000, block: 800, checkpoints: [120, 240, 470, 700, 900, 1_190])
    runHeadphone(name: "peaky", curve: EmbeddedCurves.sonyWH1000XM5, target: EmbeddedCurves.harmanOverEar2018,
                 left: s.left, right: s.right, rate: 48_000, block: 800, checkpoints: [120, 240, 470, 700, 900, 1_190])
}

// 3. No target at all, at 44.1 kHz.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 44_100 * 8, sampleRate: 44_100)
    runHeadphone(name: "notarget", curve: EmbeddedCurves.hd800s, target: nil, left: d.left, right: d.right, rate: 44_100, block: 735,
                 checkpoints: [100, 480])
}

write("headphones.txt", crate: "joseon-headphones")

// MARK: - Level at the ear (SPL estimate and dose)

func putD(_ name: String, _ values: [Double]) { lines.append(name + " " + values.map { "\($0)" }.joined(separator: " ")) }

/// The engine with an SPL estimator, one `processNow` per block.
func runSPL(name: String, curve: HeadphoneCurve, sensitivity: HeadphoneSensitivity, calibration: PlaybackCalibration,
            options: SPLDoseOptions = SPLDoseOptions(), left: [Float], right: [Float], rate: Double, block: Int,
            checkpoints: [Int], resetMeasurementAt: Int? = nil, resetDoseAt: Int? = nil) {
    let estimator = SPLEstimator(curve: curve, sensitivity: sensitivity, calibration: calibration, options: options,
                                 now: { Date(timeIntervalSince1970: 1_789_992_000) })
    let engine = AnalysisEngine()
    engine.splEstimator = estimator
    put("\(name) chain", "\(estimator.chainOffsetDB)")
    var position = 0, blockIndex = 0
    var marks = checkpoints
    while position < left.count {
        let n = min(block, left.count - position)
        let frame = engine.processNow(left: Array(left[position..<(position + n)]), right: Array(right[position..<(position + n)]),
                                      count: n, sampleRate: rate)
        position += n
        blockIndex += 1
        if blockIndex == resetMeasurementAt { engine.resetMeasurement() }
        if blockIndex == resetDoseAt { estimator.resetDose() }
        guard marks.first == blockIndex, let s = frame.spl else { continue }
        marks.removeFirst()
        let tag = "\(name)@\(blockIndex)"
        put("\(tag) name", "\(s.uncertaintyDB) [\(s.calibrationName)]")
        put("\(tag) levels", [s.levelAFast, s.levelASlow, s.levelZEardrum, s.leqATrack, s.leqASession, s.maxAFast])
        put("\(tag) bands", s.bandLevelsEardrum)
        put("\(tag) dose", [s.doseNIOSH, s.doseWHOWeekly])
        putD("\(tag) time", [s.doseSeconds, s.secondsToNIOSHLimit])
    }
    let d = estimator.doseState()
    putD("\(name) state", [d.nioshDose, d.whoWeeklyEnergySeconds, d.doseSeconds, d.sessionEnergySeconds, d.sessionSeconds])
}

// Static numbers: the weighting, the diffuse-field term, the band means of every curve, the library.
do {
    let centers = ThirdOctaveReading.nominalCentersHz
    putD("spl aweight", AWeighting.dB(atHz: centers))
    putD("spl diffuse", centers.map { SPLEstimator.diffuseFieldTermDB(centerHz: Double($0)) })
    putD("spl exact", centers.map { ThirdOctaveBands.exactCenterHz(nominalHz: Double($0)) })
    putD("spl samples", ThirdOctaveBands.sampleFrequencies(centerHz: 3_150))
    for (i, curve) in (EmbeddedCurves.headphones + EmbeddedCurves.targets).enumerated() {
        let interpolator = CurveInterpolator(curve: curve.normalizedTo1kHz())
        putD("spl response\(i)", centers.map { ThirdOctaveBands.meanDB(of: interpolator, centerHz: Double($0)) })
    }
    let library = HeadphoneSensitivityLibrary.byCurveName
    for name in library.keys.sorted() {
        put("sensitivity [\(name)]", "\(library[name]!.dbSPLPerVolt) \(library[name]!.impedanceOhms)")
    }
    let hd650 = HeadphoneSensitivityLibrary.hd650
    let c = PlaybackCalibration.fromSpecs(name: "c", dacFullScaleVrms: 2.1, ampGainDB: 11.5, volumeAttenuationDB: 23.7)
    putD("spl calibration", [c.fullScaleVrms, c.uncertaintyDB,
                             PlaybackCalibration.fromMeasuredTone(name: "t", measuredVrms: 0.123, toneLevelDBFS: -18.5).fullScaleVrms,
                             expectedSPL(forSineDBFS: -17.3, sensitivity: hd650, calibration: c),
                             expectedSPL(forSineDBFS: -17.3, atHz: 5_700, sensitivity: hd650, calibration: c, curve: EmbeddedCurves.hd800s)])
    putD("spl allowed", [70, 79.9, 85, 91.3, 104, 120].map { SPLDoseOptions.nioshAllowedSeconds(atDBA: $0) })

    // Day and week stamps over 60 years in four time zones. The Rust port has its own calendar math.
    var gregorian = Calendar(identifier: .gregorian)
    let offsets = [0, -8 * 3_600, 9 * 3_600, 5 * 3_600 + 1_800]
    for i in 0..<1_200 {
        let unix = -86_400.0 * 800 + Double(i) * 86_400 * 18.37
        let offset = offsets[i % offsets.count]
        gregorian.timeZone = TimeZone(secondsFromGMT: offset)!
        let date = Date(timeIntervalSince1970: unix)
        put("stamp \(i)", "\(unix) \(offset) \(SPLDoseState.dayStamp(for: date, calendar: gregorian)) \(SPLDoseState.weekStamp(for: date, calendar: gregorian))")
    }
}

// 1. The demo signal on the HD 650 with its published sensitivity and a measured 2 V source.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 48_000 * 20)
    runSPL(name: "hd650", curve: EmbeddedCurves.hd650, sensitivity: HeadphoneSensitivityLibrary.hd650,
           calibration: PlaybackCalibration.fromMeasuredTone(name: "2 V source", measuredVrms: 2.0, toneLevelDBFS: 0),
           left: d.left, right: d.right, rate: 48_000, block: 800, checkpoints: [50, 60, 240, 600, 1_200])
}

// 2. The loud signal on the LCD-X from specs, with the NIOSH threshold off. A dose reset, then a
//    measurement reset, then near silence.
do {
    let s = loudSignal(seconds: 40, quietAfter: 33)
    runSPL(name: "loudx", curve: EmbeddedCurves.lcdX, sensitivity: HeadphoneSensitivityLibrary.lcdX,
           calibration: PlaybackCalibration.fromSpecs(name: "DAC + amp", dacFullScaleVrms: 2.0, ampGainDB: 6, volumeAttenuationDB: 30),
           options: SPLDoseOptions(applyNIOSHThreshold: false),
           left: s.left, right: s.right, rate: 48_000, block: 800,
           checkpoints: [50, 200, 400, 1_001, 1_900, 2_000, 2_050, 2_390], resetMeasurementAt: 2_040, resetDoseAt: 1_000)
}

// 3. A quiet chain at 44.1 kHz: under the threshold, so the time to the limit is infinite.
do {
    let d = TestSignals.demoBlock(startSample: 0, count: 44_100 * 8, sampleRate: 44_100)
    runSPL(name: "quiet", curve: EmbeddedCurves.sonyWH1000XM5, sensitivity: HeadphoneSensitivityLibrary.sonyWH1000XM5,
           calibration: PlaybackCalibration(name: "phone", method: .systemVolume, fullScaleVrms: 0.02, uncertaintyDB: 3),
           left: d.left, right: d.right, rate: 44_100, block: 735, checkpoints: [60, 100, 480])
}

// 4. Left and right differ, and the signal ends in digital silence: the slow meter falls, the dose clock stops.
do {
    let rate = 48_000.0
    let n = Int(rate * 12)
    var l = TestSignals.pinkNoise(amplitude: 0.5, count: n, seed: 7)
    var r = TestSignals.sine(hz: 3_150, amplitude: 0.4, sampleRate: rate, seconds: 12)
    for i in Int(rate * 7)..<n { l[i] = 0; r[i] = 0 }
    runSPL(name: "ears", curve: EmbeddedCurves.focalUtopia, sensitivity: HeadphoneSensitivityLibrary.focalUtopia,
           calibration: PlaybackCalibration.fromMeasuredTone(name: "bench", measuredVrms: 0.05, toneLevelDBFS: -20),
           left: l, right: r, rate: rate, block: 800, checkpoints: [50, 200, 400, 425, 480, 700])
}

write("spl.txt", crate: "joseon-headphones")

// MARK: - Headphone measurement (sweep, analysis, result)

/// A fixed "headphone" for the measurement scenarios: three biquads, the same as in the Rust test.
struct GoldenBiquad {
    var b0: Double, b1: Double, b2: Double, a1: Double, a2: Double
    static func highPass(hz: Double, q: Double, rate: Double) -> GoldenBiquad {
        let w = 2 * Double.pi * hz / rate, alpha = sin(w) / (2 * q), c = cos(w), a0 = 1 + alpha
        return GoldenBiquad(b0: (1 + c) / 2 / a0, b1: -(1 + c) / a0, b2: (1 + c) / 2 / a0, a1: -2 * c / a0, a2: (1 - alpha) / a0)
    }
    static func firstOrderHighPass(hz: Double, rate: Double) -> GoldenBiquad {
        let k = tan(Double.pi * hz / rate)
        return GoldenBiquad(b0: 1 / (1 + k), b1: -1 / (1 + k), b2: 0, a1: (k - 1) / (1 + k), a2: 0)
    }
    static func peaking(hz: Double, gainDB: Double, q: Double, rate: Double) -> GoldenBiquad {
        let a = pow(10, gainDB / 40), w = 2 * Double.pi * hz / rate, alpha = sin(w) / (2 * q), c = cos(w), a0 = 1 + alpha / a
        return GoldenBiquad(b0: (1 + alpha * a) / a0, b1: -2 * c / a0, b2: (1 - alpha * a) / a0, a1: -2 * c / a0, a2: (1 - alpha / a) / a0)
    }
}

func goldenFilter(_ x: [Double], _ stages: [GoldenBiquad]) -> [Double] {
    var y = x
    for s in stages {
        var x1 = 0.0, x2 = 0.0, y1 = 0.0, y2 = 0.0
        for i in 0..<y.count {
            let input = y[i]
            let output = s.b0 * input + s.b1 * x1 + s.b2 * x2 - s.a1 * y1 - s.a2 * y2
            x2 = x1; x1 = input; y2 = y1; y1 = output
            y[i] = output
        }
    }
    return y
}

/// One recording: a delay, the filtered stimulus (with a little second harmonic), a tail, and pink noise.
func goldenRecording(_ stimulus: [Double], stages: [GoldenBiquad], gain: Double, noise: Float, seed: UInt64) -> [Double] {
    let delay = 3_733, tail = 6_000
    let driven = stimulus.map { $0 + 0.02 * $0 * $0 }
    let x = goldenFilter(driven, stages)
    var out = [Double](repeating: 0, count: delay + x.count + tail)
    for i in 0..<x.count { out[delay + i] = x[i] * gain }
    let n = TestSignals.pinkNoise(amplitude: noise, count: out.count, seed: seed)
    for i in 0..<out.count { out[i] += Double(n[i]) }
    return out
}

func putText(_ tag: String, _ texts: [String]) {
    put("\(tag) count", "\(texts.count)")
    for (i, t) in texts.enumerated() { put("\(tag) \(i)", t) }
}

func putMeasured(_ tag: String, _ m: MeasuredHeadphone) {
    put("\(tag) levels", m.curve.levelsDB)
    put("\(tag) absolute", m.absoluteMagnitudeDB)
    put("\(tag) snr", m.quality.snrDB)
    put("\(tag) spread", m.quality.agreementSpreadDB)
    let q = m.quality
    put("\(tag) quality", [q.thdPercent, q.agreementDB, q.lowestResolvedHz, q.noiseLimitedHz, q.highestReliableHz, q.windowMilliseconds,
                           q.lowestReliableHz, Float(q.runs)])
    put("\(tag) cause", q.lowLimitCause.rawValue)
    put("\(tag) method", m.method)
    putText("\(tag) warning", q.warnings)
    put("\(tag) note", m.bassRollOffNote ?? "none")
    let csv = m.csv().split(separator: "\n").map(String.init)
    putText("\(tag) csvhead", csv.filter { $0.hasPrefix("#") || $0.hasPrefix("frequency") })
    put("\(tag) csvrows", "\(csv.count) [\(csv[csv.count - 1])] [\(csv[csv.count - 120])]")
    if let d = m.derivedSensitivity {
        putD("\(tag) derived", [d.sensitivity.dbSPLPerVolt, d.sensitivity.impedanceOhms, d.uncertaintyDB, d.measuredSPLAt1kHz, d.driveVoltsRMS,
                                d.printedUncertaintyDB])
        put("\(tag) headline", "\(d.uncertaintyHeadline) [\(d.dominantTerm.name)] [\(d.rig.rawValue)]")
        put("\(tag) source", d.sensitivity.source)
        putText("\(tag) budget", d.uncertaintyTable.map { "\($0.term) | \($0.value) | \($0.detail)" })
    } else {
        put("\(tag) headline", "none")
    }
}

do {
    let rate = 48_000.0
    let headphone: [GoldenBiquad] = [.highPass(hz: 35, q: 0.707, rate: rate), .peaking(hz: 3_000, gainDB: 6, q: 2, rate: rate),
                                     .peaking(hz: 8_000, gainDB: -8, q: 3, rate: rate)]

    // The stimuli themselves.
    let sweep = SweepSignal.exponentialSweep(sampleRate: rate, seconds: 3, levelDBFS: -6)
    let pink = SweepSignal.periodicPinkNoise(sampleRate: rate, seconds: 2, levelDBFS: -6, seed: 42)
    for (tag, s) in [("sweep", sweep), ("pink", pink),
                     ("sweep441", SweepSignal.exponentialSweep(sampleRate: 44_100, startHz: 10, endHz: 30_000, seconds: 2.5, levelDBFS: -12,
                                                               fadeInSeconds: 0.05, fadeOutSeconds: 0.1))] {
        putD("stimulus \(tag)", [Double(s.samples.count), s.startHz, s.endHz, s.durationSeconds, s.highestFullEnergyHz, s.sweepRateSeconds,
                                 s.rmsLevelDBFS, s.harmonicTimeOffsetSeconds(2), s.harmonicBandCoverage(3), s.fadeInSeconds, s.fadeOutSeconds])
        putD("samples \(tag)", stride(from: 0, to: s.samples.count, by: 997).map { s.samples[$0] })
    }
    let inverse = sweep.inverseFilter()!
    putD("samples inverse", stride(from: 0, to: inverse.count, by: 1_999).map { inverse[$0] })

    // The parts of the analysis, on one recording.
    let recording = goldenRecording(sweep.samples, stages: headphone, gain: 0.5, noise: 0.004, seed: 11)
    let delay = SweepAnalysis.delaySamples(recorded: recording, reference: sweep.samples)
    putD("parts delay", [Double(delay.samples), delay.subSample, delay.confidence])
    let ir = SweepAnalysis.impulseResponse(recorded: recording, sweep: sweep)
    putD("parts ir", [Double(ir.samples.count), Double(ir.peakIndex), ir.peakSubSample, ir.noiseFloorDBRelativePeak, ir.delaySeconds,
                      Double(ir.harmonicOffsetSamples[2] ?? 0), Double(ir.harmonicOffsetSamples[3] ?? 0), Double(ir.harmonicOffsetSamples[5] ?? 0)])
    putD("parts irsamples", (-40..<200).map { ir.samples[ir.peakIndex + $0] })
    let windowed = SweepAnalysis.window(ir)
    putD("parts window", [Double(windowed.preSamples), Double(windowed.postSamples), Double(windowed.peakOffset), windowed.lowestResolvedHz])
    let thd = SweepAnalysis.harmonicDistortion(ir, sweep: sweep)
    putD("parts thd", [thd.totalPercent, thd.perHarmonicPercent[2] ?? -1, thd.perHarmonicPercent[3] ?? -1, thd.bandCoverage[2] ?? -1,
                       thd.bandCoverage[3] ?? -1])
    let grid = SweepAnalysis.standardGrid
    for smoothing in SweepAnalysis.OctaveSmoothing.allCases {
        putD("parts magnitude\(SweepAnalysis.OctaveSmoothing.allCases.firstIndex(of: smoothing)!)",
             SweepAnalysis.magnitudeResponse(ir: windowed, grid: grid, smoothing: smoothing))
    }

    // 1. A full session: three runs, a noise segment, a microphone file, a coupler file, a calibrator.
    let micText = "\"Sens Factor =-1.20dB, SERNO: 7012345\"\r\n10\t0\t0\r\n100\t0.3\t0\r\n500\t-0.2\t0\r\n1000\t0\t0\r\n3000\t0.8\t0\r\n10000\t3.0\t0\r\n20000\t6.0\t0"
    let mic = try! MicCalibration.parse(text: micText, name: "UMIK-1 7012345 (0°)")
    let coupler = try! CouplerCorrection.parse(text: "# plate\n20,3.0\n100,2.0\n1000,0.0\n8000,-4.0\n20000,-6.0", name: "Flat plate")
    putD("mic correction", mic.correctionDB(onGrid: grid))
    putD("mic header", [mic.sensFactorDB ?? -999, mic.micSensitivityDBFSPerPascal() ?? -999, mic.hasPhaseColumn ? 1 : 0, Double(mic.frequenciesHz.count)])
    let playback = PlaybackCalibration.fromMeasuredTone(name: "WA33 at 10 o'clock", measuredVrms: 0.1, toneLevelDBFS: -6)
    func full(_ stages: [GoldenBiquad], noise: Float, seeds: [UInt64], rig: MeasurementRig, level: AbsoluteLevel.Reference,
              withMic: Bool = true) -> MeasuredHeadphone {
        let session = HeadphoneMeasurement(sweep: sweep, micCalibration: withMic ? mic : nil, coupler: withMic ? coupler : nil)
        session.setNoiseSegment(goldenRecording([Double](repeating: 0, count: sweep.samples.count), stages: stages, gain: 0.5, noise: noise, seed: 900))
        for (i, seed) in seeds.enumerated() {
            let s = session.addRun(goldenRecording(sweep.samples, stages: stages, gain: 0.5, noise: noise, seed: seed), measureCrossCorrelation: i == 0)
            putD("run \(seeds.count)x\(seed)", [Double(s.delaySamples), s.delaySeconds, Double(s.crossCorrelationDelaySamples ?? -1), s.peakLevelDB,
                                              s.noiseFloorDB, s.thdPercent, s.clipped ? 1 : 0])
        }
        var context = HeadphoneMeasurement.SensitivityContext(playback: playback, absoluteLevel: AbsoluteLevel.scale(for: level), impedanceOhms: 45)
        context.rig = rig
        return session.result(name: "Golden headphone (measured)", sensitivity: context)!
    }
    let left = full(headphone, noise: 0.004, seeds: [11, 12, 13], rig: .flatPlate, level: .calibrator(readingDBFS: -32.04))
    putMeasured("full", left)

    // 2. The same headphone with a seal leak, in a noisy room, on an ear simulator, by the Sens Factor route.
    let leaky = headphone + [GoldenBiquad.firstOrderHighPass(hz: 120, rate: rate)]
    let right = full(leaky, noise: 0.05, seeds: [21, 22], rig: .earSimulator, level: .fromMicCalibration(mic))
    putMeasured("leaky", right)
    putMeasured("average", MeasuredHeadphone.average(left, right))
    for (tag, reference) in [("hd650", EmbeddedCurves.hd650), ("flat", EmbeddedCurves.flat)] {
        let a = left.bassShortfall(against: reference)!, b = right.bassShortfall(against: reference)!
        putD("shortfall \(tag)", [a.meanDifferenceDB, a.lowHz, a.highHz, Double(a.pointsUsed), a.likelyLeak ? 1 : 0,
                                  b.meanDifferenceDB, b.lowHz, b.highHz, Double(b.pointsUsed), b.likelyLeak ? 1 : 0])
        putText("shortfall \(tag)text", [a.summary, b.summary])
    }

    // 3. One bare run: no files, no noise segment, no reference, a clipped sample.
    do {
        let session = HeadphoneMeasurement(sweep: sweep)
        var loud = goldenRecording(sweep.samples, stages: leaky, gain: 0.5, noise: 0.004, seed: 31)
        loud[10_000] = 1.0
        _ = session.addRun(loud)
        putMeasured("bare", session.result(name: "bare")!)
        let none = HeadphoneMeasurement.SensitivityContext(playback: playback, absoluteLevel: AbsoluteLevel.scale(for: .fromMicCalibration(nil)), impedanceOhms: 45)
        putText("bare unavailable", [session.derivedSensitivity(none).unavailableReason ?? "derived",
                                     AbsoluteLevel.scale(for: .calibrator(readingDBFS: 3)).unavailableReason ?? "available",
                                     AbsoluteLevel.scale(for: .calibrator(readingDBFS: -31.257)).scale!.method,
                                     AbsoluteLevel.scale(for: .micSensitivity(dbFSPerPascal: -19.2)).scale!.method])
    }

    // 4. Pink noise as the stimulus, 1/6 octave, four periods.
    do {
        var options = SweepAnalysis.Options()
        options.smoothing = .sixth
        let session = HeadphoneMeasurement(sweep: pink, options: options)
        session.setNoiseSegment(goldenRecording([Double](repeating: 0, count: pink.samples.count), stages: headphone, gain: 0.5, noise: 0.002, seed: 901))
        for seed in [41, 42, 43, 44] as [UInt64] {
            _ = session.addRun(goldenRecording(pink.samples, stages: headphone, gain: 0.5, noise: 0.002, seed: seed))
        }
        putMeasured("pinkrun", session.result(name: "pink")!)
    }
}

write("measurement.txt", crate: "joseon-headphones")

// MARK: - Render (the parts with no GPU in them)

writeRenderGolden()
writeVectorscopeGolden()
writeMetersGolden()
writeTimelineGolden()
writeMiniGolden()
if CommandLine.arguments.count > 3, CommandLine.arguments[2] == "pictures" { writeReferencePictures(to: CommandLine.arguments[3]) }
