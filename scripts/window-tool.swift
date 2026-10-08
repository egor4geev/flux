// Helper for scripts/ui-scenario.sh. Needs no Accessibility permissions.
//   window-tool windows <pid>   — ids of the process's on-screen windows (for `screencapture -l`)
//   window-tool activate <pid>  — bring the app to the foreground
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let pid = Int32(args[2]) else {
    FileHandle.standardError.write("usage: window-tool windows|activate <pid>\n".data(using: .utf8)!)
    exit(2)
}

switch args[1] {
case "windows":
    let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    for window in list where (window[kCGWindowOwnerPID as String] as? Int32) == pid {
        print(window[kCGWindowNumber as String]!)
    }
case "activate":
    _ = NSRunningApplication(processIdentifier: pid)?.activate(options: [.activateAllWindows])
default:
    exit(2)
}
