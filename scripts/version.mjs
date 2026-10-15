import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import semver from "semver";

export const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (root, name) => fs.readFileSync(path.join(root, name), "utf8");
const json = (root, name) => JSON.parse(read(root, name));
const strictVersion = (value) => typeof value === "string" && /^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(value) && semver.valid(value);

function packageSection(text) {
  const section = text.match(/^\[package\]\r?\n[\s\S]*?(?=^\[|(?![\s\S]))/m)?.[0];
  if (!section) throw new Error("Cargo.toml has no [package] section");
  return section;
}
function lockSection(text, name) {
  const matches = [...text.matchAll(/^\[\[package\]\]\r?\n[\s\S]*?(?=^\[\[package\]\]|(?![\s\S]))/gm)]
    .map((match) => match[0]).filter((block) => block.match(/^name\s*=\s*"([^"]+)"/m)?.[1] === name);
  if (matches.length !== 1) throw new Error(`Cargo.lock must contain exactly one package named ${name}`);
  return matches[0];
}
function tomlVersion(section) {
  const version = section.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  if (!version) throw new Error("Missing Cargo package version");
  return version;
}
export function checkVersions(root = projectRoot, tag) {
  const pkg = json(root, "package.json"), lock = json(root, "package-lock.json");
  if (!strictVersion(pkg.version)) throw new Error("package.json version must be strict SemVer");
  const manifests = {
    "package.json": pkg.version,
    "package-lock.json": lock.version,
    "package-lock.json packages root": lock.packages?.[""]?.version,
    "src-tauri/Cargo.toml": tomlVersion(packageSection(read(root, "src-tauri/Cargo.toml"))),
    "src-tauri/Cargo.lock": tomlVersion(lockSection(read(root, "src-tauri/Cargo.lock"), pkg.name)),
    "src-tauri/tauri.conf.json": json(root, "src-tauri/tauri.conf.json").version,
  };
  for (const [name, version] of Object.entries(manifests)) {
    if (version !== pkg.version) throw new Error(`${name} version ${version ?? "missing"} does not match ${pkg.version}`);
  }
  if (tag !== undefined) {
    if (tag !== `v${pkg.version}`) throw new Error(`Release tag must be v${pkg.version}, received ${tag}`);
    releaseNotes(root, pkg.version);
  }
  return pkg.version;
}

export function releaseNotes(root = projectRoot, version = checkVersions(root)) {
  const lines = read(root, "CHANGELOG.md").split(/\r?\n/);
  const begin = lines.findIndex((line) => line.startsWith(`## [${version}]`) && /^(?:\s|$)/.test(line.slice(`## [${version}]`.length)));
  if (begin < 0) throw new Error(`CHANGELOG.md needs a ## [${version}] release section`);
  const next = lines.findIndex((line, index) => index > begin && line.startsWith("## "));
  const notes = lines.slice(begin + 1, next < 0 ? undefined : next).join("\n").trim();
  if (!notes) throw new Error(`Release notes for ${version} are empty`);
  return notes;
}

export function bumpVersion(root = projectRoot, target) {
  const current = checkVersions(root);
  const next = ["major", "minor", "patch"].includes(target) ? semver.inc(current, target) : target;
  if (!strictVersion(next)) throw new Error("Use major, minor, patch, or an explicit strict SemVer version");
  if (!semver.gt(next, current)) throw new Error(`Version must increase from ${current}`);
  const pkg = json(root, "package.json"), lock = json(root, "package-lock.json"), config = json(root, "src-tauri/tauri.conf.json");
  pkg.version = next; lock.version = next; lock.packages[""].version = next; config.version = next;
  const cargo = read(root, "src-tauri/Cargo.toml"), cargoLock = read(root, "src-tauri/Cargo.lock");
  const replaceVersion = (section) => section.replace(/^(version\s*=\s*")[^"]+(".*)$/m, `$1${next}$2`);
  const updates = new Map([
    ["package.json", JSON.stringify(pkg, null, 2) + "\n"],
    ["package-lock.json", JSON.stringify(lock, null, 2) + "\n"],
    ["src-tauri/tauri.conf.json", JSON.stringify(config, null, 2) + "\n"],
    ["src-tauri/Cargo.toml", cargo.replace(packageSection(cargo), replaceVersion(packageSection(cargo)))],
    ["src-tauri/Cargo.lock", cargoLock.replace(lockSection(cargoLock, pkg.name), replaceVersion(lockSection(cargoLock, pkg.name)))],
  ]);
  const originals = new Map([...updates.keys()].map((name) => [name, read(root, name)]));
  try {
    for (const [name, content] of updates) fs.writeFileSync(path.join(root, name), content, "utf8");
    checkVersions(root);
  } catch (error) {
    for (const [name, content] of originals) fs.writeFileSync(path.join(root, name), content, "utf8");
    throw error;
  }
  return next;
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try {
    const [command = "check", arg] = process.argv.slice(2);
    if (command === "check") {
      const tag = process.env.GITHUB_REF_TYPE === "tag" ? process.env.GITHUB_REF_NAME : undefined;
      console.log(`Version aligned: ${checkVersions(projectRoot, tag)}`);
    } else if (command === "bump") {
      console.log(`Version updated to ${bumpVersion(projectRoot, arg)}. Update CHANGELOG.md before tagging. No Git operations performed.`);
    } else if (command === "notes") console.log(releaseNotes());
    else throw new Error("Usage: version.mjs check | bump major/minor/patch/<version> | notes");
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
