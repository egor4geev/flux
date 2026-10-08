import AppKit
// App icon: a dark glass tile on the macOS icon grid (body 824 of 1024, corner radius ~22.5%),
// a highlight, a shadow and the colour logo. Writes an .iconset for `iconutil -c icns`.
//   swift scripts/app-icon.swift crates/flux-app/assets/brand/logo.svg target/release/flux.iconset
let args = CommandLine.arguments
guard args.count == 3, let logo = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write("usage: app-icon.swift <logo.svg> <out.iconset>\n".data(using: .utf8)!)
    exit(2)
}
let out = URL(fileURLWithPath: args[2])
try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

func render(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px) / 1024
    let body = NSRect(x: 100 * s, y: 100 * s, width: 824 * s, height: 824 * s)
    let tile = NSBezierPath(roundedRect: body, xRadius: 185 * s, yRadius: 185 * s)
    // Shadow under the tile, as on system icons.
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor(white: 0, alpha: 0.35)
    shadow.shadowOffset = NSSize(width: 0, height: -12 * s)
    shadow.shadowBlurRadius = 28 * s
    shadow.set()
    NSColor(srgbRed: 0.05, green: 0.06, blue: 0.09, alpha: 1).setFill()
    tile.fill()
    NSGraphicsContext.restoreGraphicsState()
    // Body: dark glass, lighter at the top and deeper at the bottom.
    NSGradient(starting: NSColor(srgbRed: 0.105, green: 0.118, blue: 0.180, alpha: 1),
               ending: NSColor(srgbRed: 0.035, green: 0.040, blue: 0.065, alpha: 1))!.draw(in: tile, angle: -90)
    // A colored glow behind the logo.
    let glow = NSGradient(starting: NSColor(srgbRed: 0.52, green: 0.56, blue: 1, alpha: 0.28),
                          ending: NSColor(srgbRed: 0.52, green: 0.56, blue: 1, alpha: 0))!
    NSGraphicsContext.saveGraphicsState()
    tile.addClip()
    glow.draw(fromCenter: NSPoint(x: body.midX, y: body.midY), radius: 0,
              toCenter: NSPoint(x: body.midX, y: body.midY), radius: 420 * s, options: [])
    NSGraphicsContext.restoreGraphicsState()
    // Border and a highlight along the top edge.
    NSColor(white: 1, alpha: 0.14).setStroke()
    tile.lineWidth = max(1, 4 * s)
    tile.stroke()
    let sheen = NSBezierPath()
    sheen.move(to: NSPoint(x: body.minX + 185 * s, y: body.maxY - 3 * s))
    sheen.line(to: NSPoint(x: body.maxX - 185 * s, y: body.maxY - 3 * s))
    NSColor(white: 1, alpha: 0.30).setStroke()
    sheen.lineWidth = max(1, 3 * s)
    sheen.stroke()
    // The logo is 62% of the body.
    let side = 824 * s * 0.62
    logo.draw(in: NSRect(x: body.midX - side / 2, y: body.midY - side / 2, width: side, height: side))
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for base in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let name = scale == 1 ? "icon_\(base)x\(base).png" : "icon_\(base)x\(base)@2x.png"
        try! render(base * scale).write(to: out.appendingPathComponent(name))
    }
}
