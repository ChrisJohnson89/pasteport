#!/usr/bin/env swift
//
//  make-icon.swift
//  Draws the Pasteport app icon and writes an .iconset for iconutil.
//
//  The icon is generated rather than checked in as a binary blob: it is a few
//  dozen lines of vector drawing, it stays reviewable in a diff, and it renders
//  crisply at every size macOS asks for instead of being upscaled from one PNG.
//
//  Usage: swift make-icon.swift <output.iconset>
//
//  Design: a passport-blue rounded tile, a clipboard silhouette, and a stamp
//  mark across it — "paste" plus "passport". Readable at 16pt, where all that
//  survives is the clipboard shape against the tile.
//

import AppKit

// MARK: - Palette

/// Deep indigo through to a lighter blue. Distinct at a glance from the yellow
/// and grey clipboard icons that macOS utilities tend toward.
let tileTop = NSColor(srgbRed: 0.29, green: 0.36, blue: 0.86, alpha: 1.0)
let tileBottom = NSColor(srgbRed: 0.16, green: 0.20, blue: 0.55, alpha: 1.0)
let boardFill = NSColor(srgbRed: 0.98, green: 0.98, blue: 1.00, alpha: 1.0)
let boardShade = NSColor(srgbRed: 0.85, green: 0.87, blue: 0.96, alpha: 1.0)
let clipMetal = NSColor(srgbRed: 0.62, green: 0.66, blue: 0.80, alpha: 1.0)
let stampInk = NSColor(srgbRed: 0.95, green: 0.35, blue: 0.35, alpha: 1.0)
let lineInk = NSColor(srgbRed: 0.55, green: 0.58, blue: 0.72, alpha: 1.0)

// MARK: - Drawing

/// Draw the icon into the current context at `size` × `size` points.
///
/// Everything is expressed as a fraction of `s`, so one routine covers 16pt
/// through 1024pt with no separate assets.
func drawIcon(size s: CGFloat) {
    guard let ctx = NSGraphicsContext.current?.cgContext else { return }

    // ---- rounded tile with a vertical gradient ----
    // macOS 11+ icon geometry: the art sits inside ~80% of the canvas with a
    // corner radius near 22% of the tile.
    let inset = s * 0.10
    let tile = CGRect(x: inset, y: inset, width: s - inset * 2, height: s - inset * 2)
    let radius = tile.width * 0.223

    let tilePath = NSBezierPath(roundedRect: tile, xRadius: radius, yRadius: radius)
    ctx.saveGState()
    tilePath.addClip()
    let gradient = NSGradient(starting: tileTop, ending: tileBottom)
    gradient?.draw(in: tile, angle: -90)

    // A soft highlight across the top third keeps the tile from looking flat.
    if let sheen = NSGradient(
        starting: NSColor(white: 1.0, alpha: 0.18),
        ending: NSColor(white: 1.0, alpha: 0.0)
    ) {
        sheen.draw(in: CGRect(x: tile.minX, y: tile.midY, width: tile.width, height: tile.height / 2),
                   angle: -90)
    }
    ctx.restoreGState()

    // ---- clipboard body ----
    let boardW = tile.width * 0.52
    let boardH = tile.height * 0.62
    let board = CGRect(
        x: tile.midX - boardW / 2,
        y: tile.midY - boardH / 2 - tile.height * 0.03,
        width: boardW,
        height: boardH
    )
    let boardRadius = boardW * 0.14

    // Drop shadow, so the board reads as sitting above the tile.
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -s * 0.012), blur: s * 0.03,
                  color: NSColor(white: 0, alpha: 0.35).cgColor)
    boardShade.setFill()
    NSBezierPath(roundedRect: board, xRadius: boardRadius, yRadius: boardRadius).fill()
    ctx.restoreGState()

    // The page itself, inset slightly so the shade reads as an edge.
    let page = board.insetBy(dx: board.width * 0.035, dy: board.height * 0.028)
    boardFill.setFill()
    NSBezierPath(roundedRect: page, xRadius: boardRadius * 0.85, yRadius: boardRadius * 0.85).fill()

    // ---- metal clip at the top ----
    let clipW = boardW * 0.42
    let clipH = boardH * 0.13
    let clip = CGRect(
        x: board.midX - clipW / 2,
        y: board.maxY - clipH * 0.55,
        width: clipW,
        height: clipH
    )
    clipMetal.setFill()
    NSBezierPath(roundedRect: clip, xRadius: clipH * 0.42, yRadius: clipH * 0.42).fill()

    // ---- text lines, standing in for clipboard history ----
    // Skipped below ~32pt: at that scale they turn into grey mush and the
    // silhouette alone is more legible.
    if s >= 32 {
        lineInk.setFill()
        let lineH = max(s * 0.012, page.height * 0.045)
        let lineX = page.minX + page.width * 0.15
        let widths: [CGFloat] = [0.70, 0.55, 0.64, 0.42]
        for (i, w) in widths.enumerated() {
            let y = page.maxY - page.height * (0.30 + CGFloat(i) * 0.155)
            let r = CGRect(x: lineX, y: y, width: page.width * w, height: lineH)
            NSBezierPath(roundedRect: r, xRadius: lineH / 2, yRadius: lineH / 2).fill()
        }
    }

    // ---- history badge ----
    // A clock in the lower right. An earlier pass used a tilted passport stamp,
    // but at icon sizes the tilt read as a scribble rather than a mark. A clock
    // says "history" immediately, which is the actual product, and it is what
    // makes this distinct in a dock otherwise full of plain clipboards.
    if s >= 32 {
        let r = tile.width * 0.155
        let center = CGPoint(x: board.maxX - board.width * 0.02,
                             y: board.minY + board.height * 0.18)

        // Punch a hole in the board so the badge reads as sitting on top rather
        // than blending into the white page behind it.
        ctx.saveGState()
        NSColor.white.setStroke()
        let halo = NSBezierPath(ovalIn: CGRect(x: center.x - r, y: center.y - r,
                                               width: r * 2, height: r * 2))
        halo.lineWidth = max(s * 0.02, r * 0.26)
        NSColor(srgbRed: 0.98, green: 0.98, blue: 1.0, alpha: 1.0).setStroke()
        halo.stroke()
        ctx.restoreGState()

        // Dial.
        let dial = CGRect(x: center.x - r, y: center.y - r, width: r * 2, height: r * 2)
        stampInk.setFill()
        NSBezierPath(ovalIn: dial).fill()

        // Hands: short to 12, long to 4 o'clock. Drawn in the page colour so
        // they stay legible against the filled dial.
        ctx.saveGState()
        ctx.translateBy(x: center.x, y: center.y)
        let hands = NSBezierPath()
        hands.lineCapStyle = .round
        hands.lineWidth = max(s * 0.013, r * 0.17)
        hands.move(to: .zero)
        hands.line(to: CGPoint(x: 0, y: r * 0.52))
        hands.move(to: .zero)
        hands.line(to: CGPoint(x: r * 0.42, y: -r * 0.24))
        NSColor(srgbRed: 1.0, green: 0.97, blue: 0.97, alpha: 1.0).setStroke()
        hands.stroke()
        ctx.restoreGState()
    }
}

/// Render one PNG at `pixels` × `pixels`.
func renderPNG(pixels: Int, to url: URL) throws {
    guard let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: pixels,
        pixelsHigh: pixels,
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bytesPerRow: 0,
        bitsPerPixel: 0
    ) else {
        throw Failure("could not allocate a \(pixels)px bitmap")
    }

    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.cgContext.setShouldAntialias(true)
    // Draw in points equal to pixels: the fractional layout scales either way.
    drawIcon(size: CGFloat(pixels))
    NSGraphicsContext.restoreGraphicsState()

    guard let data = rep.representation(using: .png, properties: [:]) else {
        throw Failure("could not encode PNG at \(pixels)px")
    }
    try data.write(to: url)
}

struct Failure: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

// MARK: - Main

let args = CommandLine.arguments
guard args.count == 2 else {
    FileHandle.standardError.write("usage: swift make-icon.swift <output.iconset>\n".data(using: .utf8)!)
    exit(2)
}

let iconset = URL(fileURLWithPath: args[1])
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

// The set iconutil expects for a complete .icns.
let variants: [(name: String, pixels: Int)] = [
    ("icon_16x16.png", 16),
    ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32),
    ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512),
    ("icon_512x512@2x.png", 1024),
]

for variant in variants {
    try renderPNG(pixels: variant.pixels, to: iconset.appendingPathComponent(variant.name))
}

print("wrote \(variants.count) sizes to \(iconset.path)")
