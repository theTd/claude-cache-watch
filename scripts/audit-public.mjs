import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { unzipSync } from "fflate";
import { projectRoot } from "./version.mjs";

const rules = [
  ["access token", /(?:sk-ant-[A-Za-z0-9_-]{20,}|sk-(?:proj-)?[A-Za-z0-9_-]{24,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|AKIA[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{30,}|xox[baprs]-[A-Za-z0-9-]{15,})/g],
  ["private key", /-----BEGIN (?:[A-Z]+ )?PRIVATE KEY-----/g],
  ["credential assignment", /(?:api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret|password)\s*[=:]\s*["'][^"'\r\n]{8,}["']/gi],
  ["credential URL", /(?:https?|postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis):\/\/[^\s/\0:@]+:[^\s/\0@]+@/gi],
  ["JWT", /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b/g],
];
const profiles = /(?:[A-Za-z]:[\\/]+Users[\\/]+|\/(?:Users|home)\/)([^\\/\s\0"<>:]+)/g;
const examples = new Set(["you", "user", "username", "example", "<user>"]);

export function scanContent(data, location = "content", binary = false) {
  const findings = [];
  let text;
  try { text = binary ? data.toString("latin1") : new TextDecoder("utf-8", { fatal: true }).decode(data); }
  catch { return [{ kind: "invalid UTF-8", location, line: 0 }]; }
  const texts = binary ? [text, data.toString("utf16le"), data.subarray(1).toString("utf16le")] : [text];
  const seen = new Set();
  for (const value of texts) {
    for (const [kind, regex] of rules) {
      regex.lastIndex = 0;
      for (const match of value.matchAll(regex)) {
        const line = binary ? 0 : value.slice(0, match.index).split("\n").length;
        const key = `${kind}:${line}`;
        if (!seen.has(key)) { seen.add(key); findings.push({ kind, location, line }); }
      }
    }
    profiles.lastIndex = 0;
    for (const match of value.matchAll(profiles)) {
      if (examples.has(match[1].toLowerCase())) continue;
      const line = binary ? 0 : value.slice(0, match.index).split("\n").length;
      const key = `profile:${line}`;
      if (!seen.has(key)) { seen.add(key); findings.push({ kind: "personal profile path", location, line }); }
    }
  }
  return findings;
}
const binaryFile = (name) => /\.(exe|dll|pdb|ico|png|jpg|jpeg|gif|webp|woff2?)$/i.test(name);

export function scanArtifact(filename) {
  if (fs.statSync(filename).size > 256 * 1024 * 1024) throw new Error("Artifact exceeds the 256 MiB audit limit");
  const data = fs.readFileSync(filename), name = path.basename(filename);
  if (!/\.zip$/i.test(name)) return scanContent(data, name, true);
  let total = 0;
  const files = unzipSync(data, { filter: (entry) => {
    total += entry.originalSize;
    if (total > 256 * 1024 * 1024) throw new Error("Archive exceeds the 256 MiB audit limit");
    return true;
  } });
  const findings = [];
  for (const [entry, content] of Object.entries(files)) {
    if (entry.endsWith("/")) continue;
    if (entry.split(/[\\/]/).includes("..") || /^[A-Za-z]:|^[\\/]/.test(entry)) throw new Error("Archive contains an unsafe member path");
    findings.push(...scanContent(Buffer.from(content), `${name}!${entry}`, binaryFile(entry)));
  }
  return findings;
}

export function auditRepository(root = projectRoot, { history = true } = {}) {
  const git = (args, encoding) => execFileSync("git", args, { cwd: root, encoding, maxBuffer: 64 * 1024 * 1024 });
  const names = git(["ls-files", "--cached", "--others", "--exclude-standard", "-z"], "utf8").split("\0").filter(Boolean);
  const findings = [], seen = new Set();
  for (const name of new Set(names)) {
    const absolute = path.join(root, name);
    if (!fs.existsSync(absolute)) continue;
    const data = fs.lstatSync(absolute).isSymbolicLink() ? Buffer.from(fs.readlinkSync(absolute)) : fs.readFileSync(absolute);
    findings.push(...scanContent(data, name, binaryFile(name)));
  }
  let commits = [];
  if (history) {
    commits = git(["rev-list", "--all"], "utf8").trim().split("\n").filter(Boolean);
    for (const commit of commits) for (const record of git(["ls-tree", "-rz", commit], "utf8").split("\0").filter(Boolean)) {
      const [metadata, name] = record.split("\t");
      const [, type, oid] = metadata.split(" ");
      if (type !== "blob" || seen.has(oid)) continue;
      seen.add(oid);
      findings.push(...scanContent(git(["cat-file", "blob", oid]), `${commit.slice(0, 7)}:${name}`, binaryFile(name)));
    }
  }
  return { findings, files: new Set(names).size, commits: commits.length, blobs: seen.size };
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try {
    const args = process.argv.slice(2), artifacts = [];
    for (let i = 0; i < args.length; i++) {
      if (args[i] !== "--artifact" || !args[i + 1]) throw new Error("Usage: audit-public.mjs [--artifact file.zip or file.exe]");
      artifacts.push(path.resolve(args[++i]));
    }
    const report = auditRepository();
    for (const artifact of artifacts) report.findings.push(...scanArtifact(artifact));
    console.log(`Audited ${report.files} candidate files, ${report.blobs} historical blobs across ${report.commits} commits, and ${artifacts.length} artifacts.`);
    console.log("Commit author/committer names and emails remain public Git metadata. Images need visual review for private data.");
    for (const finding of report.findings) console.error(`${finding.kind}: ${finding.location}${finding.line ? `:${finding.line}` : ""}`);
    if (report.findings.length) process.exitCode = 1;
    else console.log("No configured secret or personal-profile-path patterns found. This is a pattern audit, not a guarantee.");
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
