// Magnitor's native module: what only macOS can do for app.ts. Each request names its `op`:
// `check` and `install` are the updater's (Updater.swift), `trash` moves Developer Files to the
// Trash and `purge` deletes for good what it moved (Trash.swift).
import Foundation

final class MagnitorModule: ExactModule {
    override func later(_ request: [String: Any], reply: ExactReply) {
        let op = request["op"] as? String ?? ""
        let url = request["url"] as? String ?? ""
        let version = request["version"] as? String ?? ""
        let paths = request["paths"] as? [String] ?? []
        let agent = context.agent
        Task.detached {
            switch op {
            case "check": reply.send(await Updater.check(agent: agent))
            case "install": reply.send(await Updater.install(url: url, version: version))
            case "trash": reply.send(Trash.move(paths, agent: agent))
            case "purge": reply.send(Trash.purge(paths, agent: agent))
            default: reply.fail("Unknown request: \(op)")
            }
        }
    }
}

let exactModule: ExactModule.Type = MagnitorModule.self
