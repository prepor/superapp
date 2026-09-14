import CoreGraphics
import Foundation
import AppKit

if CommandLine.arguments.dropFirst().first == "--frontmost" {
    print(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0)
    exit(0)
}

let pid = Int(CommandLine.arguments[1])!
let expectedWidth = CommandLine.arguments.count > 3 ? Double(CommandLine.arguments[2]) : nil
let expectedHeight = CommandLine.arguments.count > 3 ? Double(CommandLine.arguments[3]) : nil
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as! [[String: Any]]
var best: Any? = nil
var area = 0.0
for window in windows where window[kCGWindowOwnerPID as String] as? Int == pid {
    if let bounds = window[kCGWindowBounds as String] as? [String: Any],
       let width = bounds["Width"] as? Double,
       let height = bounds["Height"] as? Double, width * height > area {
        if let w = expectedWidth, let h = expectedHeight,
           abs(width-w) > 2 || abs(height-h) > 2 { continue }
        area = width * height
        best = window[kCGWindowNumber as String]
    }
}
if let best = best { print(best); exit(0) }
exit(1)
