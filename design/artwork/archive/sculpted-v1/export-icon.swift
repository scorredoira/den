import AppKit
import ImageIO

// Export the approved raster artwork; never redraw its emblem.
func exportIcon(source: URL, output: URL, size: Int, mac: Bool) throws {
    guard let provider = CGImageSourceCreateWithURL(source as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(provider, 0, nil) else {
        throw NSError(domain: "IconExport", code: 1,
                      userInfo: [NSLocalizedDescriptionKey: "Cannot read \(source.path)"])
    }
    guard image.width == image.height else {
        throw NSError(domain: "IconExport", code: 2,
                      userInfo: [NSLocalizedDescriptionKey: "Icon master must be square"])
    }
    let alpha: CGImageAlphaInfo = mac ? .premultipliedLast : .noneSkipLast
    let context = CGContext(data: nil, width: size, height: size, bitsPerComponent: 8,
        bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: alpha.rawValue)!
    context.interpolationQuality = .high
    let edge = CGFloat(size)
    context.clear(CGRect(x: 0, y: 0, width: edge, height: edge))
    let inset = mac ? edge * 100 / 1024 : 0
    let tile = CGRect(x: inset, y: inset, width: edge - inset * 2, height: edge - inset * 2)
    if mac {
        let radius = edge * 185 / 1024
        let shape = CGPath(roundedRect: tile, cornerWidth: radius, cornerHeight: radius, transform: nil)
        context.saveGState()
        context.setShadow(offset: CGSize(width: 0, height: -edge * 10 / 1024),
                          blur: edge * 20 / 1024, color: NSColor.black.withAlphaComponent(0.25).cgColor)
        context.addPath(shape)
        context.setFillColor(NSColor.black.cgColor)
        context.fillPath()
        context.restoreGState()
        context.addPath(shape)
        context.clip()
    }
    context.draw(image, in: tile)
    let destination = CGImageDestinationCreateWithURL(output as CFURL, "public.png" as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, context.makeImage()!, nil)
    guard CGImageDestinationFinalize(destination) else {
        throw NSError(domain: "IconExport", code: 3,
                      userInfo: [NSLocalizedDescriptionKey: "Cannot write \(output.path)"])
    }
}

let directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let source = directory.deletingLastPathComponent().deletingLastPathComponent()
    .appendingPathComponent("design/artwork/icon-source.png")
let output = directory.appendingPathComponent("den.png")
try exportIcon(source: source, output: output, size: 1024, mac: true)
// Linux packages consume den.svg; embed the raster so the SVG stays self-contained.
let png = try Data(contentsOf: output).base64EncodedString()
let svg = """
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1024" height="1024" viewBox="0 0 1024 1024">
  <image width="1024" height="1024" xlink:href="data:image/png;base64,\(png)"/>
</svg>
"""
try svg.write(to: directory.appendingPathComponent("den.svg"), atomically: true, encoding: .utf8)
