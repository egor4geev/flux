// Makes a sample video of FluxGram part 4 (and the test fixture) with AVFoundation: H.264 frames
// drawn with CoreGraphics — a ball going round, a hand turning, the time's bar along the bottom
// (to see seeking) — AAC sound from a WAV, and a JPEG poster of the first frame next to it
// (`<out>.jpg`). The motion of a `gif` comes back to where it started at the end, so it loops
// without a jump; a `round` keeps inside the circle it is shown in.
//
//   swift tools/make_video.swift <out.mp4> <width> <height> <seconds> <video|round|gif> [speech.wav]

import AVFoundation
import CoreGraphics
import Foundation
import ImageIO

let args = CommandLine.arguments
guard args.count >= 6, let width = Int(args[2]), let height = Int(args[3]),
    let seconds = Double(args[4])
else {
    FileHandle.standardError.write(
        "usage: make_video <out.mp4> <width> <height> <seconds> <video|round|gif> [speech.wav]\n"
            .data(using: .utf8)!)
    exit(2)
}
let output = URL(fileURLWithPath: args[1])
let kind = args[5]
let speech = args.count > 6 ? URL(fileURLWithPath: args[6]) : nil
let fps = 30
let frameCount = Int(seconds * Double(fps))

try? FileManager.default.removeItem(at: output)
let writer = try! AVAssetWriter(outputURL: output, fileType: .mp4)
let videoInput = AVAssetWriterInput(
    mediaType: .video,
    outputSettings: [
        AVVideoCodecKey: AVVideoCodecType.h264,
        AVVideoWidthKey: width,
        AVVideoHeightKey: height,
        AVVideoCompressionPropertiesKey: [
            AVVideoAverageBitRateKey: width * height * 3,
            AVVideoMaxKeyFrameIntervalKey: fps,
            AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
        ],
    ])
videoInput.expectsMediaDataInRealTime = false
let adaptor = AVAssetWriterInputPixelBufferAdaptor(
    assetWriterInput: videoInput,
    sourcePixelBufferAttributes: [
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
        kCVPixelBufferWidthKey as String: width,
        kCVPixelBufferHeightKey as String: height,
    ])
writer.add(videoInput)

var audioInput: AVAssetWriterInput? = nil
var readerOutput: AVAssetReaderTrackOutput? = nil
if let speech = speech {
    let asset = AVURLAsset(url: speech)
    let reader = try! AVAssetReader(asset: asset)
    let trackOutput = AVAssetReaderTrackOutput(
        track: asset.tracks(withMediaType: .audio)[0],
        outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVLinearPCMBitDepthKey: 16,
            AVLinearPCMIsFloatKey: false,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
            AVSampleRateKey: 48000,
            AVNumberOfChannelsKey: 1,
        ])
    reader.add(trackOutput)
    reader.startReading()
    readerOutput = trackOutput
    let input = AVAssetWriterInput(
        mediaType: .audio,
        outputSettings: [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48000,
            AVNumberOfChannelsKey: 1,
            AVEncoderBitRateKey: 64000,
        ])
    input.expectsMediaDataInRealTime = false
    writer.add(input)
    audioInput = input
}

writer.startWriting()
writer.startSession(atSourceTime: .zero)

let space = CGColorSpace(name: CGColorSpace.sRGB)!
var poster: CGImage? = nil

func color(_ hue: Double, _ saturation: Double, _ brightness: Double) -> CGColor {
    let h = (hue - floor(hue)) * 6
    let c = brightness * saturation
    let x = c * (1 - abs(h.truncatingRemainder(dividingBy: 2) - 1))
    let (r, g, b): (Double, Double, Double) =
        h < 1 ? (c, x, 0) : h < 2 ? (x, c, 0) : h < 3 ? (0, c, x)
        : h < 4 ? (0, x, c) : h < 5 ? (x, 0, c) : (c, 0, x)
    let m = brightness - c
    return CGColor(colorSpace: space, components: [r + m, g + m, b + m, 1])!
}

func draw(_ buffer: CVPixelBuffer, _ frame: Int) {
    CVPixelBufferLockBaseAddress(buffer, [])
    defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
    let context = CGContext(
        data: CVPixelBufferGetBaseAddress(buffer), width: width, height: height,
        bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(buffer), space: space,
        bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue
            | CGBitmapInfo.byteOrder32Little.rawValue)!
    let t = Double(frame) / Double(fps)
    // A gif's motion makes whole turns in its length; the others turn every 2 s.
    let phase = kind == "gif" ? t / seconds : t / 2
    let w = Double(width)
    let h = Double(height)
    let side = min(w, h)
    let center = CGPoint(x: w / 2, y: h / 2)

    // The background: two colors sliding across.
    let gradient = CGGradient(
        colorsSpace: space,
        colors: [color(0.58 + 0.08 * sin(2 * .pi * phase), 0.55, 0.55),
                 color(0.78 + 0.08 * cos(2 * .pi * phase), 0.5, 0.35)] as CFArray,
        locations: [0, 1])!
    context.drawLinearGradient(
        gradient, start: CGPoint(x: 0, y: h), end: CGPoint(x: w, y: 0), options: [])

    // A ring, the hand turning in it, the ball going round the other way.
    let ring = side * 0.32
    context.setStrokeColor(color(0, 0, 1).copy(alpha: 0.35)!)
    context.setLineWidth(side * 0.015)
    context.strokeEllipse(
        in: CGRect(x: center.x - ring, y: center.y - ring, width: 2 * ring, height: 2 * ring))
    let angle = 2 * .pi * phase
    context.setStrokeColor(color(0.12, 0.8, 1))
    context.setLineWidth(side * 0.03)
    context.setLineCap(.round)
    context.move(to: center)
    context.addLine(
        to: CGPoint(x: center.x + ring * 0.85 * sin(angle), y: center.y + ring * 0.85 * cos(angle)))
    context.strokePath()
    let ball = side * 0.07
    let ballCenter = CGPoint(
        x: center.x + ring * cos(-angle * 2), y: center.y + ring * sin(-angle * 2))
    context.setFillColor(color(0, 0, 1))
    context.fillEllipse(
        in: CGRect(x: ballCenter.x - ball, y: ballCenter.y - ball, width: 2 * ball, height: 2 * ball))

    // The time: a bar along the bottom (inside the circle for a round one).
    if kind != "gif" {
        let inset = kind == "round" ? side * 0.25 : side * 0.06
        let y = kind == "round" ? h * 0.2 : side * 0.06
        let length = w - 2 * inset
        context.setFillColor(color(0, 0, 1).copy(alpha: 0.3)!)
        context.fill(CGRect(x: inset, y: y, width: length, height: side * 0.02))
        context.setFillColor(color(0, 0, 1))
        context.fill(
            CGRect(x: inset, y: y, width: length * t / seconds, height: side * 0.02))
    }
    if frame == 0 {
        poster = context.makeImage()
    }
}

let group = DispatchGroup()
var frame = 0
group.enter()
videoInput.requestMediaDataWhenReady(on: DispatchQueue(label: "video")) {
    while videoInput.isReadyForMoreMediaData {
        if frame >= frameCount {
            videoInput.markAsFinished()
            group.leave()
            return
        }
        var buffer: CVPixelBuffer? = nil
        CVPixelBufferPoolCreatePixelBuffer(nil, adaptor.pixelBufferPool!, &buffer)
        draw(buffer!, frame)
        adaptor.append(
            buffer!, withPresentationTime: CMTime(value: CMTimeValue(frame), timescale: CMTimeScale(fps)))
        frame += 1
    }
}
if let audioInput = audioInput, let readerOutput = readerOutput {
    let end = CMTime(seconds: seconds, preferredTimescale: 48000)
    group.enter()
    audioInput.requestMediaDataWhenReady(on: DispatchQueue(label: "audio")) {
        while audioInput.isReadyForMoreMediaData {
            guard let sample = readerOutput.copyNextSampleBuffer(),
                CMSampleBufferGetPresentationTimeStamp(sample) < end
            else {
                audioInput.markAsFinished()
                group.leave()
                return
            }
            audioInput.append(sample)
        }
    }
}
group.wait()
let finished = DispatchSemaphore(value: 0)
writer.finishWriting { finished.signal() }
finished.wait()
guard writer.status == .completed else {
    print("failed: \(String(describing: writer.error))")
    exit(1)
}

let posterURL = output.deletingPathExtension().appendingPathExtension("jpg")
let destination = CGImageDestinationCreateWithURL(
    posterURL as CFURL, "public.jpeg" as CFString, 1, nil)!
CGImageDestinationAddImage(
    destination, poster!, [kCGImageDestinationLossyCompressionQuality: 0.85] as CFDictionary)
CGImageDestinationFinalize(destination)
print("\(output.path): \(width)×\(height), \(seconds) s, \(frameCount) frames; \(posterURL.path)")
