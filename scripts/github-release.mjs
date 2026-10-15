import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { pathToFileURL } from "node:url";
import semver from "semver";
import { unzipSync } from "fflate";
import { checkVersions, projectRoot, releaseNotes } from "./version.mjs";
import { scanArtifact } from "./audit-public.mjs";

const hash = (data) => crypto.createHash("sha256").update(data).digest("hex");
export function prepareRelease(root = projectRoot, tag) {
  const version = checkVersions(root, tag);
  const output = path.join(root, "dist/releases");
  const manifest = JSON.parse(fs.readFileSync(path.join(output, "release-manifest.json"), "utf8"));
  const stem = `claude-cache-watch-${version}-windows-x64-portable`;
  if (manifest.version !== version || manifest.tag !== `v${version}` || manifest.archive !== `${stem}.zip` || manifest.checksum !== `${stem}.sha256`) {
    throw new Error("Release manifest does not match the checked-out version");
  }
  if (!/^[a-f0-9]{40}$/.test(manifest.source_commit || "") || typeof manifest.source_dirty !== "boolean") throw new Error("Release source provenance is missing");
  const archivePath = path.join(output, manifest.archive), checksumPath = path.join(output, manifest.checksum);
  const archive = fs.readFileSync(archivePath), checksum = fs.readFileSync(checksumPath);
  if (!/^[a-f0-9]{64}$/.test(manifest.sha256) || hash(archive) !== manifest.sha256
      || checksum.toString("utf8") !== `${manifest.sha256}  ${manifest.archive}\n`) throw new Error("Release checksum verification failed");
  if (scanArtifact(archivePath).length) throw new Error("Portable archive failed the public-data audit");
  const members = unzipSync(archive);
  const files = Object.keys(members).filter((name) => !name.endsWith("/")).sort();
  if (JSON.stringify(files) !== JSON.stringify(["CHANGELOG.md", "Claude Cache Watch.exe", "README.md"])) throw new Error("Portable archive contains unexpected files");
  const exe = Buffer.from(members["Claude Cache Watch.exe"]);
  const pe = exe.length >= 64 ? exe.readUInt32LE(60) : -1;
  if (exe.toString("ascii", 0, 2) !== "MZ" || pe < 64 || pe + 6 > exe.length
      || exe.toString("binary", pe, pe + 4) !== "PE\0\0" || exe.readUInt16LE(pe + 4) !== 0x8664) throw new Error("Portable archive does not contain a Windows x64 executable");
  return { version, tag: `v${version}`, notes: releaseNotes(root, version), prerelease: semver.prerelease(version) !== null, sourceCommit: manifest.source_commit, sourceDirty: manifest.source_dirty,
    assets: [{ name: manifest.archive, data: archive, type: "application/zip", sha256: hash(archive) },
      { name: manifest.checksum, data: checksum, type: "text/plain", sha256: hash(checksum) }] };
}

export async function publishRelease(release, env = process.env, fetchImpl = fetch) {
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF_TYPE !== "tag" || env.GITHUB_REF_NAME !== release.tag) throw new Error("Publishing requires a matching GitHub Actions tag run");
  if (release.sourceDirty || !/^[a-f0-9]{40}$/.test(env.GITHUB_SHA || "") || env.GITHUB_SHA !== release.sourceCommit) throw new Error("Release source commit does not match the clean workflow checkout");
  if (!env.GITHUB_TOKEN || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(env.GITHUB_REPOSITORY || "")) throw new Error("GitHub repository/token is unavailable");
  const api = new URL(env.GITHUB_API_URL || "https://api.github.com");
  if (api.protocol !== "https:") throw new Error("GitHub API must use HTTPS");
  const base = `${api.href.replace(/\/$/, "")}/repos/${env.GITHUB_REPOSITORY}`;
  const headers = { Authorization: `Bearer ${env.GITHUB_TOKEN}`, Accept: "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28" };
  async function request(url, options = {}, allowed = []) {
    const response = await fetchImpl(url, { ...options, headers: { ...headers, ...options.headers }, redirect: "error", signal: AbortSignal.timeout(60000) });
    if (!response.ok && !allowed.includes(response.status)) throw new Error(`GitHub request failed with HTTP ${response.status}`);
    return response;
  }
  async function verifyTag() {
    let object = (await (await request(`${base}/git/ref/tags/${encodeURIComponent(release.tag)}`)).json()).object;
    for (let depth = 0; object?.type === "tag" && depth < 10; depth++) {
      if (!/^[a-f0-9]{40}$/.test(object.sha || "")) throw new Error("Invalid annotated tag object");
      object = (await (await request(`${base}/git/tags/${object.sha}`)).json()).object;
    }
    if (object?.type !== "commit" || object.sha !== env.GITHUB_SHA) throw new Error("Release tag moved or does not match the built commit");
  }
  await verifyTag();
  const found = await request(`${base}/releases/tags/${encodeURIComponent(release.tag)}`, {}, [404]);
  let current = found.status === 404 ? null : await found.json();
  // The tag endpoint only exposes published releases. Drafts require an authenticated listing.
  if (!current) for (let page = 1; ; page++) {
    const listing = await (await request(`${base}/releases?per_page=100&page=${page}`)).json();
    if (!Array.isArray(listing)) throw new Error("Invalid release listing");
    current = listing.find((item) => item.draft && item.tag_name === release.tag) || null;
    if (current || listing.length < 100) break;
  }
  const marker = "<!-- claude-cache-watch-managed-release -->";
  if (current && !current.draft) {
    const matching = release.assets.every((asset) => current.assets?.some((a) => a.name === asset.name && a.digest === `sha256:${asset.sha256}`));
    if (!matching) throw new Error("Published release assets differ or cannot be verified; publish a new version instead of replacing them");
    return current.html_url;
  }
  if (current && !current.body?.includes(marker)) throw new Error("An unmanaged draft already exists; refusing to modify it");
  const allowedNames = new Set(release.assets.map((asset) => asset.name));
  const checkAssets = (assets) => {
    if (!Array.isArray(assets) || assets.some((asset) => !allowedNames.has(asset.name)) || new Set(assets.map((asset) => asset.name)).size !== assets.length) throw new Error("Draft contains unexpected or duplicate assets; refusing to publish them");
  };
  if (current) checkAssets(current.assets);
  const body = `${marker}\n${release.notes}`;
  const payload = { tag_name: release.tag, name: `Claude Cache Watch ${release.tag}`, body, draft: true, prerelease: release.prerelease };
  if (!current) current = await (await request(`${base}/releases`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(payload) })).json();
  const upload = new URL(current.upload_url.replace(/\{.*$/, ""));
  if (upload.protocol !== "https:" || ![api.host, "uploads.github.com"].includes(upload.host)) throw new Error("Unexpected GitHub upload host");
  for (const asset of release.assets) {
    const existing = current.assets?.find((a) => a.name === asset.name);
    if (existing) {
      if (existing.digest !== `sha256:${asset.sha256}`) throw new Error("Draft asset differs or cannot be verified; remove the conflicting draft asset before retrying");
      continue;
    }
    const url = new URL(upload); url.searchParams.set("name", asset.name);
    await request(url, { method: "POST", headers: { "Content-Type": asset.type }, body: asset.data });
  }
  const ready = await (await request(`${base}/releases/${current.id}`)).json();
  checkAssets(ready.assets);
  if (!ready.draft || ready.tag_name !== release.tag || !ready.body?.includes(marker) || !release.assets.every((asset) => ready.assets.some((a) => a.name === asset.name && a.digest === `sha256:${asset.sha256}`))) throw new Error("Draft changed or uploaded assets cannot be verified");
  await verifyTag();
  const published = await (await request(`${base}/releases/${current.id}`, { method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ name: payload.name, body, draft: false, prerelease: release.prerelease, make_latest: release.prerelease ? "false" : "legacy" }) })).json();
  return published.html_url;
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  try {
    const tag = process.env.GITHUB_REF_TYPE === "tag" ? process.env.GITHUB_REF_NAME : undefined;
    const release = prepareRelease(projectRoot, tag);
    if (process.env.GITHUB_ACTIONS === "true" && (release.sourceDirty || release.sourceCommit !== process.env.GITHUB_SHA)) throw new Error("Package was not built from this clean workflow commit");
    if (process.argv.includes("--check")) console.log(`Release package verified: ${release.tag}`);
    else console.log(`Release published: ${await publishRelease(release)}`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
