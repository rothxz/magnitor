// Magnitor's updater: checks GitHub Releases for a newer version, and installs it in place.
//
// `check` reads the latest release of this repository and finds its `-macos.zip` asset, which
// the CI release job attaches (notarized and stapled by `exact release`). `install` downloads
// that zip, unpacks it, and installs it only if its code signature satisfies the running app's
// own designated requirement: same bundle id, signed by the same Developer ID team. A download
// that is tampered with, or signed by anyone else, is refused. The new bundle then replaces the
// running one and Magnitor relaunches.
//
// A build that is not a signed release (ad hoc, as a local or CI test build is) reports
// `canInstall: false`: its designated requirement is its own hash, which no update can meet.
// Under the agent (tests), `check` answers at once without reaching the network. Module.swift
// routes app.ts's requests here.
import AppKit
import Foundation
import Security

enum Updater {
    static let repository = "rothxz/magnitor"
    static let latestRelease = URL(string: "https://api.github.com/repos/\(repository)/releases/latest")!
    static let downloadPrefix = "https://github.com/\(repository)/releases/download/"
    static let assetSuffix = "-macos.zip"

    static var currentVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
    }

    // MARK: Check

    static func check(agent: Bool) async -> [String: Any] {
        var result: [String: Any] = [
            "current": currentVersion, "latest": "", "available": false, "notes": "",
            "page": "", "download": "", "canInstall": false, "error": "",
        ]
        if agent { return result }
        do {
            var request = URLRequest(url: latestRelease, timeoutInterval: 20)
            request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
            request.setValue("Magnitor-Updater", forHTTPHeaderField: "User-Agent")
            let (data, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            // No release published yet.
            if status == 404 { return result }
            guard status == 200, let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                result["error"] = "GitHub answered \(status). Try again later."
                return result
            }
            let tag = json["tag_name"] as? String ?? ""
            let latest = tag.hasPrefix("v") ? String(tag.dropFirst()) : tag
            let assets = json["assets"] as? [[String: Any]] ?? []
            let zip = assets.first { ($0["name"] as? String ?? "").hasSuffix(assetSuffix) }?["browser_download_url"] as? String ?? ""
            result["latest"] = latest
            result["notes"] = json["body"] as? String ?? ""
            result["page"] = json["html_url"] as? String ?? ""
            result["download"] = zip
            result["available"] = isNewer(latest, than: currentVersion)
            result["canInstall"] = zip.hasPrefix(downloadPrefix) && installBlocker() == nil
        } catch {
            result["error"] = "Couldn’t reach GitHub: \(error.localizedDescription)"
        }
        return result
    }

    /// Whether dotted version `a` is newer than `b` (1.10.0 > 1.9.2).
    static func isNewer(_ a: String, than b: String) -> Bool {
        let parse = { (v: String) in v.split(separator: ".").map { Int($0.prefix { $0.isNumber }) ?? 0 } }
        let x = parse(a), y = parse(b)
        guard !x.isEmpty else { return false }
        for i in 0..<max(x.count, y.count) {
            let l = i < x.count ? x[i] : 0, r = i < y.count ? y[i] : 0
            if l != r { return l > r }
        }
        return false
    }

    /// Why this copy can't replace itself, or nil when it can.
    static func installBlocker() -> String? {
        guard let me = ownStaticCode(), teamIdentifier(me) != nil else {
            return "This copy of Magnitor isn’t a signed release, so it can’t update itself."
        }
        let bundle = Bundle.main.bundleURL
        if bundle.path.contains("/AppTranslocation/") {
            return "Move Magnitor to your Applications folder, then open it from there to update it."
        }
        let folder = bundle.deletingLastPathComponent().path
        if !FileManager.default.isWritableFile(atPath: folder) {
            return "Magnitor can’t write to \(folder). Download the update from GitHub instead."
        }
        return nil
    }

    // MARK: Install

    static func install(url: String, version: String) async -> [String: Any] {
        func refuse(_ message: String) -> [String: Any] { ["ok": false, "message": message] }
        guard url.hasPrefix(downloadPrefix), let source = URL(string: url) else {
            return refuse("That download isn’t one of Magnitor’s GitHub releases.")
        }
        if let blocker = installBlocker() { return refuse(blocker) }
        guard isNewer(version, than: currentVersion) else {
            return refuse("Magnitor \(currentVersion) is already up to date.")
        }
        let files = FileManager.default
        let work = files.temporaryDirectory.appendingPathComponent("magnitor-update-\(UUID().uuidString)")
        defer { try? files.removeItem(at: work) }
        do {
            try files.createDirectory(at: work, withIntermediateDirectories: true)
            let (downloaded, response) = try await URLSession.shared.download(from: source)
            guard (response as? HTTPURLResponse)?.statusCode == 200 else {
                return refuse("The download failed. Try again later.")
            }
            let zip = work.appendingPathComponent("update.zip")
            try files.moveItem(at: downloaded, to: zip)
            let unpacked = work.appendingPathComponent("unpacked")
            try run("/usr/bin/ditto", ["-x", "-k", zip.path, unpacked.path])
            guard let app = try files.contentsOfDirectory(at: unpacked, includingPropertiesForKeys: nil)
                .first(where: { $0.pathExtension == "app" }) else {
                return refuse("The download doesn’t hold an app.")
            }
            if let problem = verify(app, version: version) { return refuse(problem) }

            // Stage beside the running app, on its volume, then swap the two in one step.
            let current = Bundle.main.bundleURL
            let staged = current.deletingLastPathComponent()
                .appendingPathComponent(".\(current.deletingPathExtension().lastPathComponent)-\(version).app")
            try? files.removeItem(at: staged)
            try run("/usr/bin/ditto", [app.path, staged.path])
            _ = try files.replaceItemAt(current, withItemAt: staged)
            relaunch(current)
            return ["ok": true, "message": "Installed Magnitor \(version). Relaunching…"]
        } catch {
            return refuse("Couldn’t install the update: \(error.localizedDescription)")
        }
    }

    /// Nil when `app` is Magnitor `version`, signed as the running app is; otherwise why not.
    static func verify(_ app: URL, version: String) -> String? {
        guard let me = ownStaticCode() else { return "Magnitor can’t read its own signature." }
        var requirement: SecRequirement?
        guard SecCodeCopyDesignatedRequirement(me, [], &requirement) == errSecSuccess, let requirement else {
            return "Magnitor can’t read its own signing requirement."
        }
        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(app as CFURL, [], &code) == errSecSuccess, let code else {
            return "The update isn’t a signed app, so it wasn’t installed."
        }
        let flags = SecCSFlags(rawValue: kSecCSCheckAllArchitectures | kSecCSCheckNestedCode | kSecCSStrictValidate)
        guard SecStaticCodeCheckValidity(code, flags, requirement) == errSecSuccess else {
            return "The update isn’t signed by Magnitor’s developer, so it wasn’t installed."
        }
        let bundle = Bundle(url: app)
        guard bundle?.bundleIdentifier == Bundle.main.bundleIdentifier else {
            return "The update is a different app, so it wasn’t installed."
        }
        guard bundle?.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String == version else {
            return "The update’s version doesn’t match the release, so it wasn’t installed."
        }
        return nil
    }

    /// Opens the app at `app` once this process has exited, then quits.
    static func relaunch(_ app: URL) {
        let pid = ProcessInfo.processInfo.processIdentifier
        let waiter = Process()
        waiter.executableURL = URL(fileURLWithPath: "/bin/sh")
        waiter.arguments = ["-c", "while /bin/kill -0 \(pid) 2>/dev/null; do /bin/sleep 0.2; done; /usr/bin/open \"$0\"", app.path]
        try? waiter.run()
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 800_000_000)
            NSApplication.shared.terminate(nil)
        }
    }

    // MARK: Helpers

    static func ownStaticCode() -> SecStaticCode? {
        var me: SecCode?
        guard SecCodeCopySelf([], &me) == errSecSuccess, let me else { return nil }
        var code: SecStaticCode?
        guard SecCodeCopyStaticCode(me, [], &code) == errSecSuccess else { return nil }
        return code
    }

    static func teamIdentifier(_ code: SecStaticCode) -> String? {
        var info: CFDictionary?
        guard SecCodeCopySigningInformation(code, SecCSFlags(rawValue: kSecCSSigningInformation), &info) == errSecSuccess,
              let info = info as? [String: Any] else { return nil }
        return info[kSecCodeInfoTeamIdentifier as String] as? String
    }

    struct ToolFailed: LocalizedError {
        let errorDescription: String?
    }

    static func run(_ tool: String, _ arguments: [String]) throws {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = arguments
        try process.run()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            throw ToolFailed(errorDescription: "\((tool as NSString).lastPathComponent) exited with \(process.terminationStatus)")
        }
    }
}

