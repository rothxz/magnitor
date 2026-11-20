// Developer Files' Trash: each path (from the home folder, or absolute inside a folder Settings
// adds to the walk, as app.ts checked it) moved as Finder moves it, with FileManager's
// `trashItem`, so Finder's Put Back returns it. A path that can't be moved (in use, protected,
// already gone) is left out of `moved`; the others still go.
//
// Each move's place in the Trash is kept for the session, so `purge` deletes for good what
// Magnitor moved there and nothing else of the Trash. macOS may keep an app out of the Trash
// without Full Disk Access: then `denied` says so and the view sends the person to Finder.
//
// Under the agent (tests) nothing is moved or deleted: the home there is the repository's
// fixture folder. Every path that exists is answered as moved, and every one moved as deleted.
import Foundation

enum Trash {
    private static let lock = NSLock()
    /// Where each path Magnitor moved now lies in the Trash, by the path as the view named it.
    nonisolated(unsafe) private static var placed: [String: URL] = [:]
    nonisolated(unsafe) private static var agentMoved: Set<String> = []

    static func move(_ paths: [String], agent: Bool) -> [String: Any] {
        let home = agent
            ? URL(fileURLWithPath: ProcessInfo.processInfo.environment["EXACT_AGENT_HOME"] ?? NSHomeDirectory())
            : FileManager.default.homeDirectoryForCurrentUser
        var moved: [String] = []
        for path in paths {
            // app.ts refuses any other; checked again here, where the files are.
            guard !path.isEmpty, !path.split(separator: "/").contains("..") else { continue }
            let url = path.hasPrefix("/") ? URL(fileURLWithPath: path, isDirectory: true) : home.appendingPathComponent(path, isDirectory: true)
            guard FileManager.default.fileExists(atPath: url.path) else { continue }
            if agent {
                moved.append(path)
                lock.withLock { _ = agentMoved.insert(path) }
                continue
            }
            do {
                var result: NSURL?
                try FileManager.default.trashItem(at: url, resultingItemURL: &result)
                moved.append(path)
                if let result = result as URL? {
                    lock.withLock { placed[path] = result }
                }
            } catch {
                print("Magnitor: couldn’t move \(path) to the Trash: \(error.localizedDescription)")
            }
        }
        return ["moved": moved]
    }

    /// Delete for good the items `move` put in the Trash this session, by the paths it moved.
    static func purge(_ paths: [String], agent: Bool) -> [String: Any] {
        var deleted: [String] = []
        var denied = false
        for path in paths {
            if agent {
                if lock.withLock({ agentMoved.remove(path) }) != nil { deleted.append(path) }
                continue
            }
            guard let url = lock.withLock({ placed[path] }) else { continue }
            do {
                try FileManager.default.removeItem(at: url)
                deleted.append(path)
                lock.withLock { _ = placed.removeValue(forKey: path) }
            } catch let error as CocoaError where error.code == .fileNoSuchFile {
                // Put back, or emptied already.
                lock.withLock { _ = placed.removeValue(forKey: path) }
            } catch let error as CocoaError where error.code == .fileReadNoPermission || error.code == .fileWriteNoPermission {
                denied = true
            } catch {
                let posix = (error as NSError).underlyingErrors.first as NSError?
                if posix?.domain == NSPOSIXErrorDomain && (posix?.code == Int(EPERM) || posix?.code == Int(EACCES)) {
                    denied = true
                }
                print("Magnitor: couldn’t delete \(path) from the Trash: \(error.localizedDescription)")
            }
        }
        return ["deleted": deleted, "denied": denied]
    }
}
