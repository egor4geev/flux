import AppKit
// Контактный лист значков: каждый SVG — в 64 px (форма) и в 16/32 px (как в дереве на 1x/2x).
//   swift scripts/icon-sheet.swift crates/flux-app/assets/icons sheet.png [фильтр имени]
// AppKit размывает значки с <mask> (TS, JS, Rust, git); resvg в gpui рисует их чётко.
let dir = CommandLine.arguments[1]
let out = CommandLine.arguments[2]
let filter = CommandLine.arguments.count > 3 ? CommandLine.arguments[3] : ""
let files = try! FileManager.default.contentsOfDirectory(atPath: dir)
    .filter { $0.hasSuffix(".svg") && (filter.isEmpty || $0.contains(filter)) }.sorted()
let cols = 6
let cw: CGFloat = 200, ch: CGFloat = 120
let rows = (files.count + cols - 1) / cols
let W = Int(CGFloat(cols) * cw), H = Int(CGFloat(rows) * ch)
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: W, pixelsHigh: H, bitsPerSample: 8,
    samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSColor.white.setFill()
NSRect(x: 0, y: 0, width: W, height: H).fill()
for (i, f) in files.enumerated() {
    let col = i % cols, row = i / cols
    let x = CGFloat(col) * cw, y = CGFloat(H) - CGFloat(row + 1) * ch
    guard let img = NSImage(contentsOfFile: dir + "/" + f) else { continue }
    NSColor(white: 0.93, alpha: 1).setFill()
    NSRect(x: x + 8, y: y + 30, width: 72, height: 72).fill()
    // сетка 16×16 под крупным значком
    NSColor(white: 0.85, alpha: 1).setStroke()
    for k in 0...4 { let o = CGFloat(k) * 16
        NSBezierPath.strokeLine(from: NSPoint(x: x + 12 + o, y: y + 34), to: NSPoint(x: x + 12 + o, y: y + 98))
        NSBezierPath.strokeLine(from: NSPoint(x: x + 12, y: y + 34 + o), to: NSPoint(x: x + 76, y: y + 34 + o)) }
    img.draw(in: NSRect(x: x + 12, y: y + 34, width: 64, height: 64))
    img.draw(in: NSRect(x: x + 96, y: y + 66, width: 32, height: 32))
    img.draw(in: NSRect(x: x + 140, y: y + 74, width: 16, height: 16))
    let name = f.replacingOccurrences(of: ".svg", with: "") as NSString
    name.draw(at: NSPoint(x: x + 8, y: y + 8), withAttributes: [.font: NSFont.systemFont(ofSize: 13), .foregroundColor: NSColor.black])
}
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
print("\(files.count) icons -> \(out)")
