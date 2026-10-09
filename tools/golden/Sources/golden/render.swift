import AppKit
import Foundation
import JoseonCore
@testable import JoseonRender

// Reference values of the render code that has no GPU in it: colors, lookup tables, the log axis,
// the resampling of a spectrum onto plot points, and the number formats.

func rgba(_ c: SIMD4<Float>) -> [Float] { [c.x, c.y, c.z, c.w] }
func rgb(_ c: SIMD3<Float>) -> [Float] { [c.x, c.y, c.z] }

func writeRenderGolden() {
    // Palette, normal and high contrast, default theme and a changed theme.
    var custom = Theme()
    custom.background = SIMD4(0.10, 0.02, 0.05, 1); custom.grid = SIMD4(0.5, 0.5, 0.2, 0.40); custom.text = SIMD4(0.9, 0.9, 0.7, 1)
    custom.panel = SIMD4(0.2, 0.1, 0.1, 1); custom.warn = SIMD4(0.9, 0.9, 0.1, 1)
    for (tag, theme, contrast) in [("normal", Theme(), false), ("contrast", Theme(), true), ("custom", custom, false), ("customhc", custom, true)] {
        let p = Palette(theme: theme, highContrast: contrast)
        let fields: [SIMD4<Float>] = [p.background, p.panel, p.plot, p.grid, p.gridStrong, p.gridMajor, p.gridMinor, p.rms, p.text, p.textDim,
                                      p.textFaint, p.accent, p.warn, p.danger, p.good, p.left, p.right, p.spectrumLeft, p.spectrumRight,
                                      p.peakHold, p.average, p.side, p.hpResponse, p.hpTarget, p.hpAtEar, p.splBand, p.cursorGhost, p.track]
        put("palette \(tag)", fields.flatMap(rgba))
        put("lut spectrum\(tag)", p.spectrumLUT().flatMap(rgb))
        put("lut heat\(tag)", p.heatLUT().flatMap(rgb))
        put("lut scope\(tag)", p.scopeLUT(count: 64).flatMap(rgb))
    }
    let hz: [Float] = [1, 10, 20, 55, 100, 440, 1_000, 2_500, 9_999, 16_000, 24_000, 40_000]
    put("color spectrum", hz.flatMap { rgb(Palette.spectrumColor(atHz: $0)) })
    put("color oklab", [SIMD3<Float>(0, 0, 0), SIMD3(1, 1, 1), SIMD3(0.2, 0.5, 0.9), SIMD3(0.96, 0.22, 0.30)].flatMap {
        rgb(ColorMap.oklab(fromSRGB: $0)) + rgb(ColorMap.srgb(fromOKLab: ColorMap.oklab(fromSRGB: $0)))
    })

    // The log axis.
    let axis = LogAxis(minHz: 10, maxHz: 24_000)
    put("axis position", (LogAxis.labeled + LogAxis.minor + [5, 0, 30_000]).map { axis.position($0) })
    put("axis frequency", stride(from: Float(-0.1), through: 1.1, by: 0.05).map { axis.frequency($0) })

    // Resampling. A spectrum with a slope, a narrow peak and a notch on the analyzer's log grid.
    for (tag, bins, points, lo, hi) in [("up", 240, 900, Float(10), Float(24_000)), ("down", 1_024, 300, Float(10), Float(24_000)),
                                        ("near", 512, 480, Float(20), Float(20_000)), ("inside", 512, 700, Float(100), Float(5_000)),
                                        ("outside", 300, 500, Float(2), Float(60_000))] {
        let frequencies: [Float] = (0..<bins).map { 10 * pow(2400, Float($0) / Float(bins - 1)) }
        var src: [Float] = frequencies.map { -30 - 12 * log2($0 / 100) / 3 }
        src[bins / 3] += 28; src[bins / 3 + 1] += 14; src[bins * 2 / 3] -= 40
        for i in 0..<bins where i % 7 == 0 { src[i] += Float(i % 5) - 2 }
        let a = LogAxis(minHz: lo, maxHz: hi)
        var out = [Float](repeating: 0, count: points)
        CurveResampler.resample(src, frequencies: frequencies, axis: a, minDB: -96, maxDB: 0, offsetDB: 3, into: &out, count: points)
        put("resample \(tag)", out)
        var viaTable = [Float](repeating: 0, count: points)
        ResampleTable(frequencies: frequencies, axis: a, count: points)!.apply(src, minDB: -90, maxDB: 6, offsetDB: -1.5, into: &viaTable)
        put("table \(tag)", viaTable)
    }

    // Number formats.
    let values: [Float] = [0, -0.04, 0.04, -0.05, 0.05, 1, -1, 12.345, -12.345, 99.95, -119, -119.5, -120, .infinity, -.infinity, .nan, 0.5, 1.5, 2.5,
                           -0.5, 1234.5678, -0.0004]
    for (tag, digits, signed) in [("d1", 1, false), ("d1s", 1, true), ("d0", 0, false), ("d2s", 2, true)] {
        putText("fmt db\(tag)", values.map { Fmt.db($0, digits: digits, signed: signed) })
        putText("fmt number\(tag)", values.map { Fmt.number($0, digits: digits, signed: signed) })
    }
    let freqs: [Float] = [0, -5, .nan, 7.9, 8, 19.96, 20, 27.5, 440, 441.37, 999.95, 1_000, 1_500, 2_000, 9_999.9, 9_999.96, 10_000, 12_345, 20_000, 24_000, 30_000, 30_001]
    putText("fmt hz", freqs.map { Fmt.hz($0) })
    putText("fmt axis", freqs.filter { $0.isFinite && $0 >= 0 }.map { Fmt.axisHz($0) })
    putText("fmt note", freqs.map { f in Fmt.note(forHz: f).map { "\($0.name) \($0.cents)" } ?? "none" })
    putText("fmt cents", [Float(0), 0.4, -0.4, 0.5, -0.5, 12.3, -49.6, 50].map { Fmt.cents($0) })
    putText("fmt pretty", ["C#4", "A4", "F#-1", "##"].map { Fmt.prettyNote($0) })
    putText("fmt bands", BandLabels.short + BandLabels.two + [Fmt.minus, Fmt.dash])
    write("render.txt", crate: "joseon-render")
    writeSyntheticGolden()
}

func every(_ step: Int, _ v: [Float]) -> [Float] { stride(from: 0, to: v.count, by: step).map { v[$0] } }

func putFrame(_ tag: String, _ f: AnalysisFrame) {
    let s = f.spectrum
    for (name, v) in [("mid", s.mid), ("left", s.left), ("right", s.right), ("side", s.side), ("hold", s.peakHold), ("average", s.average)] {
        put("\(tag) \(name)", every(5, v))
    }
    put("\(tag) peak", "\(f.peak.frequencyHz) \(f.peak.levelDB) \(f.peak.cents) [\(f.peak.noteName)]")
    putText("\(tag) top", f.topPeaks.map { "\($0.frequencyHz) \($0.levelDB) \($0.cents) [\($0.noteName)]" })
    let l = f.loudness
    put("\(tag) numbers", f.bands.values + [l.momentaryLUFS, l.shortTermLUFS, l.integratedLUFS, l.momentaryMaxLUFS, l.shortTermMaxLUFS,
                                            l.loudnessRangeLU, l.truePeakLeftDBTP, l.truePeakRightDBTP, l.truePeakMaxDBTP, l.rmsLeftDB, l.rmsRightDB,
                                            l.plrDB, l.psrDB, Float(l.clipCount), Float(l.measuredSeconds), f.lowestStrongHz,
                                            f.stereo.correlation, f.stereo.balance, f.stereo.width, Float(f.hostTime - 1000)]
        + f.stereo.bandCorrelation + f.stereo.bandBalance)
    put("\(tag) scope", stride(from: 0, to: f.stereo.scopePoints.count, by: 13).flatMap { [f.stereo.scopePoints[$0].x, f.stereo.scopePoints[$0].y] })
    if let h = f.headphone {
        put("\(tag) hp", every(5, h.responseDB) + every(5, h.targetDB) + every(5, h.predictedAtEarDB))
        putText("\(tag) flags", h.stressFlags.map { "\($0.id) | \($0.severity.rawValue) | \($0.title) | \($0.plotLabel) | \($0.frequencyRangeHz!.lowerBound) \($0.frequencyRangeHz!.upperBound)" })
    }
    if let t = f.thirdOctave, let e = f.spl {
        put("\(tag) third", t.left + t.right)
        put("\(tag) spl", [e.uncertaintyDB, e.levelAFast, e.levelASlow, e.levelZEardrum, e.leqATrack, e.leqASession, e.maxAFast, e.doseNIOSH,
                           e.doseWHOWeekly, Float(e.doseSeconds), Float(e.secondsToNIOSHLimit)] + e.bandLevelsEardrum)
        put("\(tag) splname", e.calibrationName)
    }
}

func writeSyntheticGolden() {
    var rich = SyntheticFrames.Options()
    rich.includeHeadphone = true; rich.includeSPL = true; rich.hot = true
    var small = SyntheticFrames.Options()
    small.binCount = 512; small.levelDB = -9; small.seed = 77; small.scopePointCount = 300; small.framesPerSecond = 30
    for (tag, options) in [("plain", SyntheticFrames.Options()), ("rich", rich), ("small", small)] {
        let frames = SyntheticFrames.sequence(count: 400, options: options)
        for index in [0, 1, 59, 150, 399] { putFrame("\(tag)\(index)", frames[index]) }
    }
    let silence = SyntheticFrames.silence(count: 3)
    put("silence info", "\(silence.count) \(silence[2].hostTime) \(silence[2].isSilent) \(silence[2].spectrum.mid.count) [\(silence[2].stream!.deviceName)]")
    let plain = SyntheticFrames.sequence(count: 90)
    let added = SyntheticFrames.addingDemoSPL(to: plain + silence)
    for index in [0, 89, 92] { putFrame("added\(index)", added[index]) }
    write("synthetic.txt", crate: "joseon-render")
    writeSpectrogramGolden()
}

/// The history of the spectrogram, read back from its CPU copy: plain frames and layered frames.
func writeSpectrogramGolden() {
    // The time aligner alone: a ramp in every row, pushed at an uneven frame rate.
    do {
        let rows = 64
        let aligner = SpectrogramTimeAligner(rows: rows)
        aligner.configure(minHz: 20, maxHz: 20_000, sampleRate: 48_000)
        var out = [Float](repeating: 0, count: rows)
        var t = 0.0
        for i in 0..<140 {
            t += i % 5 == 0 ? 0.021 : 0.0167
            let values = (0..<rows).map { Float(0.3 + 0.2 * sin(Double($0) * 0.3 + t * 9)) }
            values.withUnsafeBufferPointer { aligner.push($0.baseAddress!, time: t) }
            if i % 20 == 19 {
                aligner.compose(centerTime: t - aligner.latency(minBox: 1.0 / 60), minBox: 1.0 / 60, into: &out)
                put("aligner \(i)", out)
            }
        }
        putD("aligner numbers", [aligner.latency(minBox: 1.0 / 60), aligner.earliestCenter(minBox: 1.0 / 60) ?? -1, aligner.delay(row: 0, minBox: 0.02),
                                 aligner.delay(row: 40, minBox: 0.02), aligner.delay(row: 63, minBox: 0.02), aligner.newestTime, aligner.firstTime ?? -1])
        let timing = SpectrumTiming(sampleRate: 44_100)
        putD("aligner timing", [timing.window.0, timing.window.1, timing.window.2, timing.hop.0, timing.hop.1, timing.hop.2,
                                timing.centerLag(atHz: 150), timing.centerLag(atHz: 1_000), timing.hop(atHz: 2_500)])
        put("aligner weights", [20, 150, 200, 260, 1_000, 1_900, 2_000, 3_000, 10_000].flatMap { (f: Float) -> [Float] in
            let w = SpectrumTiming.weights(atHz: f); return [w.low, w.mid, w.high] })
    }
    let hz: [Float] = [25, 43.8, 110, 220, 440, 1_000, 3_100, 8_000, 15_000]
    func history(_ tag: String, _ frames: [AnalysisFrame], seconds: Double) {
        var settings = OffscreenRenderer.Settings(); settings.historySeconds = seconds
        let session = try! OffscreenRenderer.Session(panel: .spectrogram, size: CGSize(width: 900, height: 360), scale: 2, theme: Theme(), settings: settings)
        try! session.feed(frames)
        let r = session.renderer as! SpectrogramRenderer
        putD("\(tag) state", [Double(r.topDB), r.latencySeconds, r.newestColumnTime, r.usesLayers ? 1 : 0, r.columnSeconds])
        for back in [0, 1, 5, 20, 60, 200, 600, 1_199] {
            put("\(tag) back\(back)", hz.map { r.historyLevel(columnsBack: back, hz: $0) ?? -999 })
        }
        put("\(tag) ticks", r.legendTicksForTesting)
        if let column = r.historyColumn(secondsAgo: 1.5) {
            put("\(tag) column", [Float(column.id)] + every(16, column.slice.midDB))
        } else {
            put("\(tag) column", "none")
        }
    }
    history("plain", SyntheticFrames.sequence(count: 600), seconds: 20)
    history("short", SyntheticFrames.sequence(count: 400), seconds: 5)
    history("layers", SyntheticFrames.layeredClicks(seconds: 6), seconds: 8)
    let layered = SyntheticFrames.layeredClicks(seconds: 2, clickTimes: [0.5, 1.2], framesPerSecond: 30, binCount: 512)
    putFrame("clicks", layered[40])
    put("clicks layers", layered[40].spectrum.midLayers.flatMap { every(9, $0.levelsDB) + every(9, $0.weights) + [$0.latencySeconds] })
    write("spectrogram.txt", crate: "joseon-render")
}

/// Synthetic frames with two one-sided tones: one in the right channel that breathes, one low in the left channel.
func tonedFrames(_ count: Int) -> [AnalysisFrame] {
    var frames = SyntheticFrames.sequence(count: count)
    for k in 0..<frames.count {
        let n = frames[k].spectrum.left.count
        let hi = n * 7 / 10, lo = n / 5
        for i in 0..<n {
            let a = Float(i - hi), b = Float(i - lo)
            frames[k].spectrum.right[i] += 34 * exp(-a * a / 6) * (k % 90 < 60 ? 1 : 0.2)
            frames[k].spectrum.left[i] += 28 * exp(-b * b / 10)
        }
    }
    return frames
}

func writeVectorscopeGolden() {
    // The pan field alone.
    do {
        let field = PanField()
        for (k, f) in tonedFrames(240).enumerated() {
            field.update(f.spectrum, dt: k % 7 == 0 ? 0.021 : 1.0 / 60, sampleRate: 48_000)
            if k % 40 == 39 {
                let n = field.count
                put("field \(k) pan", every(3, Array(UnsafeBufferPointer(start: field.pan, count: n))))
                put("field \(k) level", every(3, Array(UnsafeBufferPointer(start: field.levelDB, count: n))))
                put("field \(k) tonal", field.tonal.map { Float($0) })
                put("field \(k) dropped", field.dropped.map { Float($0) })
                put("field \(k) confidence", every(2, field.confidence))
                put("field \(k) peaks", field.tonalPeaks.map { Float($0) })
                put("field \(k) light", stride(from: 0, to: n, by: 5).map { field.light($0, topDB: -17, bottomDB: -71) })
            }
        }
        put("field spread", field.spread.map { Float($0) })
    }
    put("balance", [-1, -0.504, -0.19, -0.004, 0, 0.005, 0.0051, 0.33, 0.995, 1].map { VectorscopeRenderer.balanceTextForGolden($0) }.joined(separator: "|"))
    let cases: [(String, VectorscopeMode, CGSize, Float?)] = [
        ("scope-wide", .lissajous, CGSize(width: 900, height: 360), nil),
        ("scope-tall", .lissajous, CGSize(width: 360, height: 520), nil),
        ("scope-keys", .lissajous, CGSize(width: 420, height: 300), 1_000),
        ("scope-small", .lissajous, CGSize(width: 260, height: 240), nil),
        ("scope-table-cursor", .lissajous, CGSize(width: 1000, height: 420), 5_000),
        ("pan-wide", .panSpectrum, CGSize(width: 900, height: 420), 440),
        ("pan-tall", .panSpectrum, CGSize(width: 380, height: 560), nil),
        ("pan-small", .panSpectrum, CGSize(width: 300, height: 200), nil),
    ]
    let frames = tonedFrames(200)
    for (tag, mode, size, cursorHz) in cases {
        var settings = OffscreenRenderer.Settings(); settings.vectorscopeMode = mode
        if let hz = cursorHz { settings.cursor = PanelCursor(frequencyHz: hz, source: .spectrum, isPinned: true) }
        let session = try! OffscreenRenderer.Session(panel: .vectorscope, size: size, scale: 2, theme: Theme(), settings: settings)
        let r = session.renderer as! VectorscopeRenderer
        r.keepsSplatsForTesting = true
        try! session.feed(frames)
        _ = try! session.snapshotPNG()
        let arrangement: Float
        switch r.arrangement { case .tableBeside: arrangement = 0; case .stripUnder: arrangement = 1; case .keysBeside: arrangement = 2; case .keysUnder: arrangement = 3 }
        func rect(_ c: CGRect?) -> [Float] { c.map { [Float($0.minX), Float($0.minY), Float($0.width), Float($0.height)] } ?? [-1, -1, -1, -1] }
        let g = r.panGeometryForTesting
        put("\(tag) layout", [arrangement] + rect(r.fieldForTesting) + [Float(g.centerX), Float(g.half)] + rect(r.panAxisRectForTesting) + rect(r.panLegendForTesting)
            + [100, 1_000, 10_000].map { Float(r.panYForTesting(hz: $0)) })
        put("\(tag) gain", [r.gain])
        putText("\(tag) items", r.cursorItems().map { "\($0.rank) \($0.text)" })
        put("\(tag) value", r.accessibilityValueText)
        put("\(tag) label", r.accessibilityLabelText)
        put("\(tag) splatCount", [Float(r.lastSplats.count)])
        put("\(tag) splats", stride(from: 0, to: r.lastSplats.count, by: 9).flatMap { [r.lastSplats[$0].x, r.lastSplats[$0].y, r.lastSplats[$0].z, r.lastSplats[$0].w] })
        put("\(tag) cursorPan", [r.cursorPan() ?? -9])
    }
    write("vectorscope.txt", crate: "joseon-render")
}

extension VectorscopeRenderer {
    static func balanceTextForGolden(_ b: Float) -> String {
        let r = PanelRenderer.make(kind: .vectorscope, ctx: RenderContext.shared!, theme: Theme()) as! VectorscopeRenderer
        return r.balanceText(b, silent: false)
    }
}

typealias MetersCase = (String, Bool, CGSize, Float?, DoseStandard, Float?)
let metersCases: [MetersCase] = [
    ("wide", false, CGSize(width: 1280, height: 560), nil, .nioshDaily, nil),
    ("wide-ear", true, CGSize(width: 1280, height: 560), -14, .nioshDaily, 1_000),
    ("tall-ear", true, CGSize(width: 1100, height: 700), -23, .whoWeekly, nil),
    ("low-ear", true, CGSize(width: 900, height: 380), nil, .nioshDaily, nil),
    ("strip", true, CGSize(width: 700, height: 420), -16, .nioshDaily, 100),
    ("row", true, CGSize(width: 700, height: 220), nil, .whoWeekly, nil),
    ("compact", false, CGSize(width: 380, height: 300), -14, .nioshDaily, 5_000),
    ("narrow-ear", true, CGSize(width: 330, height: 200), nil, .nioshDaily, nil),
]

func metersSettings(_ c: MetersCase) -> OffscreenRenderer.Settings {
    var settings = OffscreenRenderer.Settings()
    settings.targetLUFS = c.3; settings.doseStandard = c.4
    if let hz = c.5 { settings.cursor = PanelCursor(frequencyHz: hz, source: .spectrum, isPinned: true) }
    return settings
}

func metersFrames(_ c: MetersCase) -> [AnalysisFrame] {
    var options = SyntheticFrames.Options()
    options.includeSPL = c.1; options.includeHeadphone = c.1
    return SyntheticFrames.sequence(count: 420, options: options)
}

func writeMetersGolden() {
    putD("stepped", [-5, 0, 299, 300, 7_199, 7_200, 8_100, 35_999, 36_000, 39_600, 400_000, .infinity].map { Double(EarShown.steppedMinutes($0)) })
    put("timeText", [-2, -1, 0, 5, 65, 600, 5_999, 6_000].map { EarShown.timeText($0) }.joined(separator: "|"))
    put("uncertainty", [0, 0.4, 2, 3.5, 3.6, 4.49, -1, .nan].map { EarUncertaintyText.total($0) + "/" + EarUncertaintyText.term($0, unit: false) }.joined(separator: "|"))
    for c in metersCases {
        let tag = c.0
        let session = try! OffscreenRenderer.Session(panel: .meters, size: c.2, scale: 2, theme: Theme(), settings: metersSettings(c))
        try! session.feed(metersFrames(c))
        _ = try! session.snapshotPNG()
        let r = session.renderer as! MetersRenderer
        func rect(_ c: CGRect) -> [Float] { [Float(c.minX), Float(c.minY), Float(c.width), Float(c.height)] }
        let mode: Float
        switch r.earMode { case .none: mode = 0; case .column: mode = 1; case .strip: mode = 2; case .row: mode = 3 }
        put("\(tag) layout", rect(r.loud) + rect(r.read) + rect(r.peak) + rect(r.band) + rect(r.bandBars) + rect(r.spark) + rect(r.ear) + rect(r.earBar)
            + [r.compact ? 1 : 0, mode, Float(r.bigFontSize), Float(r.clipLampCenter.x), Float(r.clipLampCenter.y)] + rect(r.clipIndicatorRect) + rect(r.stripBar))
        let plan = r.earPlan
        put("\(tag) plan", [Float(plan.contentX), Float(plan.captionY), Float(plan.numeralBase), Float(plan.numeralSize), Float(plan.y85), plan.showsLabel85 ? 1 : 0,
                            Float(plan.doseTop ?? -1)] + plan.rows.map { Float($0) })
        if let e = r.earShown {
            put("\(tag) shown", "\(e.slow) \(e.fast) \(e.leqTrack) \(e.leqSession) \(e.maxFast) \(e.doseNIOSH) \(e.doseWHO) \(e.leftNIOSH) \(e.leftWHO) [\(e.calibrationName)] [\(e.uncertaintyText)]")
        } else {
            put("\(tag) shown", "none")
        }
        put("\(tag) dose", "\(r.doseValue.percent) \(r.doseValue.minutesLeft) \(r.doseCaption)")
        let h = r.historyForTesting
        put("\(tag) historyM", every(7, h.momentary))
        put("\(tag) historyS", every(7, h.shortTerm))
        putText("\(tag) items", r.cursorItems().map { "\($0.rank) \($0.text)" })
        put("\(tag) value", r.accessibilityValueText)
        put("\(tag) cap", [r.capColor.x, r.capColor.y, r.capColor.z, r.capColor.w])
    }
    let p = Palette(theme: Theme(), highContrast: false)
    put("deltaColor", [-3, -1.04, -1.06, 0, 1.04, 1.06, 4].flatMap { (d: Float) -> [Float] in let c = MetersRenderer.targetDeltaColor(d, p); return [c.x, c.y, c.z, c.w] })
    write("meters.txt", crate: "joseon-render")
}

typealias TimelineCase = (String, CGSize, Int, Bool, Float?, Double?, CGPoint?, Bool)
/// tag, size, window seconds, ear lane, target, linked cursor (seconds ago, pinned), hover, high contrast
let timelineCases: [TimelineCase] = [
    ("wide", CGSize(width: 1100, height: 360), 900, false, nil, nil, nil, false),
    ("ear", CGSize(width: 1100, height: 440), 1800, true, -14, nil, nil, false),
    ("narrow", CGSize(width: 600, height: 300), 900, true, nil, nil, nil, false),
    ("strip", CGSize(width: 900, height: 150), 600, false, -16, nil, CGPoint(x: 500, y: 60), false),
    ("cursor", CGSize(width: 1100, height: 300), 900, false, nil, 330, nil, true),
    ("hover", CGSize(width: 1000, height: 400), 1800, true, nil, nil, CGPoint(x: 420, y: 150), false),
    ("short", CGSize(width: 800, height: 240), 60, false, nil, 700, nil, false),
]

func timelineSettings(_ c: TimelineCase) -> OffscreenRenderer.Settings {
    var settings = OffscreenRenderer.Settings()
    settings.session = SyntheticFrames.demoSession(minutes: 30)
    settings.timelineWindowSeconds = c.2; settings.timelineShowLevelAtEar = c.3; settings.targetLUFS = c.4
    settings.timelineTimeZone = TimeZone(secondsFromGMT: 7_200)
    if let ago = c.5 { settings.cursor = PanelCursor(frequencyHz: 440, secondsAgo: ago, source: .spectrogram, isPinned: true) }
    settings.hover = c.6; settings.increaseContrast = c.7
    return settings
}

func writeTimelineGolden() {
    let zone = TimeZone(secondsFromGMT: 7_200)!
    for (tag, levelA) in [("session", true), ("plain", false)] {
        let s = SyntheticFrames.demoSession(minutes: tag == "session" ? 30 : 7, includeLevelA: levelA)
        putD("\(tag) counts", [Double(s.samples.count), Double(s.events.count), Double(s.revision)])
        for i in stride(from: 0, to: s.samples.count, by: 13) {
            let x = s.samples[i]
            putD("\(tag) sample \(i)", [x.time, x.date.timeIntervalSince1970, Double(x.momentaryMaxLUFS), Double(x.shortTermLUFS), Double(x.truePeakDBTP), Double(x.correlation),
                                         Double(x.bands[0]), Double(x.bands[3]), Double(x.bands[7]), Double(x.levelA ?? -999), x.isSilent ? 1 : 0])
        }
        putText("\(tag) events", s.events.map { "\($0.id) \($0.kind) \($0.time) \($0.date.timeIntervalSince1970) \($0.value) [\($0.label)] [\($0.detail)]" })
    }
    let snapshot = SyntheticFrames.demoSession(minutes: 30)
    for window in [60, 300, 900, 1_800] {
        let m = TimelineModel(snapshot: snapshot, windowSeconds: window)
        let tag = "model \(window)"
        putD("\(tag) counts", [Double(m.samples.count), Double(m.events.count), m.newestTime, Double(m.hiddenSpans), Double(m.spanRows), m.hasLevelA ? 1 : 0, m.oldestTime, m.leftEdgeTime])
        putText("\(tag) spans", m.spans.map { "\($0.start) \($0.end) \($0.row) \($0.isOpen) [\($0.title)] [\($0.flagID)]" })
        putD("\(tag) silences", m.silences.flatMap { [$0.lowerBound, $0.upperBound] })
        putD("\(tag) gaps", m.gaps.flatMap { [$0.time, $0.skippedSeconds, Double($0.sampleAfter)] })
        let st = m.stats
        putD("\(tag) stats", [Double(st.shortTermLow ?? -999), Double(st.shortTermHigh ?? -999), Double(st.momentaryMax ?? -999), Double(st.truePeakMax ?? -999), Double(st.levelAMax ?? -999),
                              Double(st.clipEvents), Double(st.clippedRuns), Double(st.overs), Double(st.flags), Double(st.tracks)])
        put("\(tag) value", m.accessibilityValue)
        for step in [1, 5] { putD("\(tag) minutes \(step)", m.minuteMarks(stepMinutes: step, timeZone: zone).flatMap { [$0.time, $0.date.timeIntervalSince1970] }) }
        putD("\(tag) index", [-50, 0, 1, 900.2, 1_500, 1_799.5, 1_800, 1_801.9, 1_802.1].map { Double(m.sampleIndex(atTime: $0) ?? -1) })
    }
    let date = Date(timeIntervalSince1970: 1_790_019_900 - 3_600 * 7 + 67)
    put("format", [TimelineFormat.clock(date, seconds: true, timeZone: zone), TimelineFormat.clock(date, seconds: false, timeZone: TimeZone(secondsFromGMT: -18_000)!),
                   TimelineFormat.ago(0.4), TimelineFormat.ago(42), TimelineFormat.ago(192), TimelineFormat.duration(20), TimelineFormat.duration(188),
                   TimelineRenderer.clipCountText(0), TimelineRenderer.clipCountText(3), TimelineRenderer.overCountText(1), TimelineRenderer.overCountText(2)].joined(separator: "|"))
    for c in timelineCases {
        let tag = c.0
        let session = try! OffscreenRenderer.Session(panel: .timeline, size: c.1, scale: 2, theme: Theme(), settings: timelineSettings(c))
        _ = try! session.snapshotPNG()
        let r = session.renderer as! TimelineRenderer
        put("\(tag) lanes", r.lanes.flatMap { l -> [Float] in
            let k: Float
            switch l.kind { case .loudness: k = 0; case .peak: k = 1; case .ear: k = 2; case .tone: k = 3 }
            return [k, Float(l.rect.minX), Float(l.rect.minY), Float(l.rect.width), Float(l.rect.height), Float(l.captionY)]
        })
        put("\(tag) layout", [Float(r.plotX.lowerBound), Float(r.plotX.upperBound), Float(r.axisYForTesting), Float(r.spansTopForTesting), r.loudnessRange.top, r.loudnessRange.bottom,
                              Float(r.focusIndex ?? -1), Float(r.contentVersion > 0 ? 1 : 0)])
        put("\(tag) marks", r.minuteMarks().flatMap { [Float($0.x), $0.labeled ? 1 : 0] })
        putText("\(tag) items", r.cursorItems().map { "\($0.rank) \($0.tone) \($0.swatch == nil ? "-" : "swatch") \($0.text)" })
        putText("\(tag) lines", r.focusIndex.map { r.readoutLines($0) } ?? [])
        putText("\(tag) label", r.hoverLabel()?.lines ?? [])
        put("\(tag) value", r.accessibilityValueText)
        put("\(tag) spanColors", ["sub-bass", "treble-peak", "other"].flatMap { (id: String) -> [Float] in let c = r.spanColor(id); return [c.x, c.y, c.z, c.w] })
    }
    write("timeline.txt", crate: "joseon-render")
}

let miniCases: [(String, CGSize, CGFloat)] = [("menu", CGSize(width: 64, height: 18), 2), ("narrow", CGSize(width: 44, height: 18), 1),
                                              ("icon", CGSize(width: 16, height: 16), 2), ("strip", CGSize(width: 150, height: 40), 2)]

func writeMiniGolden() {
    let frames = SyntheticFrames.sequence(count: 200)
    for (tag, size, scale) in miniCases {
        let r = MiniSpectrumRenderer()
        let n = r.columnCount(widthPoints: size.width, pixelWidth: Int((size.width * scale).rounded()))
        put("mini \(tag) columns", [Float(n)])
        for (k, f) in frames.enumerated() {
            _ = r.cgImage(for: f, size: size, scale: scale)
            if k == 0 || k % 40 == 39 {
                put("mini \(tag) \(k) db", Array(r.columnDB.prefix(n)))
                put("mini \(tag) \(k) shown", Array(r.shown.prefix(n)))
                put("mini \(tag) \(k) ceiling", [r.ceilingDB ?? -999])
            }
        }
        // The same frame again: no time passed. Then silence, and a jump of the clock.
        _ = r.cgImage(for: frames[199], size: size, scale: scale)
        put("mini \(tag) again", Array(r.shown.prefix(n)))
        let quiet = AnalysisFrame(hostTime: frames[199].hostTime + 0.1, spectrum: SpectrumReading.silent(binCount: 1024))
        _ = r.cgImage(for: quiet, size: size, scale: scale)
        put("mini \(tag) quiet", Array(r.shown.prefix(n)))
        var late = frames[10]; late.hostTime = frames[199].hostTime + 30
        _ = r.cgImage(for: late, size: size, scale: scale)
        put("mini \(tag) late", Array(r.shown.prefix(n)) + [r.ceilingDB ?? -999])
    }
    put("mini tilt", [0, 3, 4.5, 6].map { MiniSpectrumRenderer.tilt(engineTilt: $0) })
    write("mini.txt", crate: "joseon-render")
}

func writeMiniPictures(to directory: String) {
    let frames = SyntheticFrames.sequence(count: 200)
    for (tag, size, scale) in miniCases {
        let r = MiniSpectrumRenderer()
        r.accentColor = NSColor(srgbRed: 1.0, green: 0.58, blue: 0.10, alpha: 1)
        var image: CGImage?
        for f in frames { image = r.cgImage(for: f, size: size, scale: scale) }
        guard let image else { continue }
        let rep = NSBitmapImageRep(cgImage: image)
        try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: directory).appendingPathComponent("mac-mini-\(tag).png"))
    }
}

/// Review pictures of the macOS panels on the synthetic frames, for a side-by-side with the Rust panels.
/// Not golden files: `swift run golden crates pictures <directory>` writes them, nothing reads them in a test.
func writeReferencePictures(to directory: String) {
    var rich = SyntheticFrames.Options()
    rich.includeHeadphone = true; rich.includeSPL = true
    var spl = OffscreenRenderer.Settings(); spl.levelAxis = .dBSPL
    var contrast = OffscreenRenderer.Settings(); contrast.increaseContrast = true
    var cursor = OffscreenRenderer.Settings(); cursor.cursor = PanelCursor(frequencyHz: 440, source: .spectrogram, isPinned: true)
    var hover = OffscreenRenderer.Settings(); hover.hover = CGPoint(x: 400, y: 200)
    let jobs: [(String, SyntheticFrames.Options, CGSize, OffscreenRenderer.Settings)] = [
        ("spectrum-plain", SyntheticFrames.Options(), CGSize(width: 900, height: 420), OffscreenRenderer.Settings()),
        ("spectrum-headphone", rich, CGSize(width: 1100, height: 560), OffscreenRenderer.Settings()),
        ("spectrum-spl", rich, CGSize(width: 1100, height: 560), spl),
        ("spectrum-cursor", rich, CGSize(width: 1100, height: 560), cursor),
        ("spectrum-hover", SyntheticFrames.Options(), CGSize(width: 700, height: 360), hover),
        ("spectrum-compact", SyntheticFrames.Options(), CGSize(width: 380, height: 190), OffscreenRenderer.Settings()),
        ("spectrum-contrast", rich, CGSize(width: 900, height: 420), contrast),
    ]
    for (name, frames, seconds) in [("spectrogram-plain", SyntheticFrames.sequence(count: 900), 20.0),
                                    ("spectrogram-layers", SyntheticFrames.layeredClicks(seconds: 7), 8.0)] {
        var settings = OffscreenRenderer.Settings(); settings.historySeconds = seconds
        let data = try! OffscreenRenderer.png(panel: .spectrogram, frames: frames, size: CGSize(width: 900, height: 360), scale: 2, theme: Theme(), settings: settings)
        try! data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("mac-\(name).png"))
    }
    var pan = OffscreenRenderer.Settings(); pan.vectorscopeMode = .panSpectrum
    var panCursor = pan; panCursor.cursor = PanelCursor(frequencyHz: 440, source: .spectrum, isPinned: true)
    var scopeCursor = OffscreenRenderer.Settings(); scopeCursor.cursor = PanelCursor(frequencyHz: 1_000, source: .spectrum, isPinned: true)
    let scopes: [(String, CGSize, OffscreenRenderer.Settings)] = [
        ("scope-wide", CGSize(width: 900, height: 360), OffscreenRenderer.Settings()),
        ("scope-tall", CGSize(width: 360, height: 520), OffscreenRenderer.Settings()),
        ("scope-keys", CGSize(width: 420, height: 300), scopeCursor),
        ("scope-small", CGSize(width: 260, height: 240), OffscreenRenderer.Settings()),
        ("pan-wide", CGSize(width: 900, height: 420), panCursor),
        ("pan-tall", CGSize(width: 380, height: 560), pan),
    ]
    for (name, size, settings) in scopes {
        let data = try! OffscreenRenderer.png(panel: .vectorscope, frames: tonedFrames(200), size: size, scale: 2, theme: Theme(), settings: settings)
        try! data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("mac-\(name).png"))
    }
    writeMiniPictures(to: directory)
    for c in timelineCases {
        let data = try! OffscreenRenderer.png(panel: .timeline, frames: [], size: c.1, scale: 2, theme: Theme(), settings: timelineSettings(c))
        try! data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("mac-timeline-\(c.0).png"))
    }
    for c in metersCases {
        let data = try! OffscreenRenderer.png(panel: .meters, frames: metersFrames(c), size: c.2, scale: 2, theme: Theme(), settings: metersSettings(c))
        try! data.write(to: URL(fileURLWithPath: directory).appendingPathComponent("mac-meters-\(c.0).png"))
    }
    for (name, options, size, settings) in jobs {
        let frames = SyntheticFrames.sequence(count: 240, options: options)
        let data = try! OffscreenRenderer.png(panel: .spectrum, frames: frames, size: size, scale: 2, theme: Theme(), settings: settings)
        let url = URL(fileURLWithPath: directory).appendingPathComponent("mac-\(name).png")
        try! data.write(to: url)
        print("wrote \(url.path)")
    }
}
