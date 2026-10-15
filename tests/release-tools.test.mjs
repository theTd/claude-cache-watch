import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { zipSync } from "fflate";
import { checkVersions, bumpVersion, releaseNotes, projectRoot } from "../scripts/version.mjs";
import { scanContent, scanArtifact } from "../scripts/audit-public.mjs";
import { prepareRelease, publishRelease } from "../scripts/github-release.mjs";

const manifests = ["package.json", "package-lock.json", "src-tauri/Cargo.toml", "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json"];
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "cache-watch-release-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const name of [...manifests, "CHANGELOG.md", "LICENSE"]) {
    fs.mkdirSync(path.dirname(path.join(root, name)), { recursive: true }); fs.copyFileSync(path.join(projectRoot, name), path.join(root, name));
  }
  return root;
}
function packageFixture(root, extra = {}) {
  const version = checkVersions(root), output = path.join(root, "dist/releases"); fs.mkdirSync(output, { recursive: true });
  const exe = Buffer.alloc(128); exe.write("MZ"); exe.writeUInt32LE(64, 60); exe.write("PE\0\0", 64, "binary"); exe.writeUInt16LE(0x8664, 68);
  const zip = zipSync({ "Claude Cache Watch.exe": exe, "README.md": Buffer.from("Public example"), "CHANGELOG.md": Buffer.from("Release notes"), "LICENSE": fs.readFileSync(path.join(root, "LICENSE")), ...extra });
  const stem = `claude-cache-watch-${version}-windows-x64-portable`, sha256 = crypto.createHash("sha256").update(zip).digest("hex");
  fs.writeFileSync(path.join(output, `${stem}.zip`), zip);
  fs.writeFileSync(path.join(output, `${stem}.sha256`), `${sha256}  ${stem}.zip\n`);
  fs.writeFileSync(path.join(output, "release-manifest.json"), JSON.stringify({ version, tag: `v${version}`, archive: `${stem}.zip`, checksum: `${stem}.sha256`, sha256, source_commit: "1".repeat(40), source_dirty: false }));
  return output;
}

for (const [format, newline] of [["LF", "\n"], ["CRLF", "\r\n"]]) test(`SemVer bumps synchronize every manifest and preserve dependency versions (${format})`, (t) => {
  const root = fixture(t);
  for (const name of ["src-tauri/Cargo.toml", "src-tauri/Cargo.lock"]) {
    const file = path.join(root, name);
    fs.writeFileSync(file, fs.readFileSync(file, "utf8").replace(/\r?\n/g, newline));
  }
  const old = checkVersions(root), oldLock = fs.readFileSync(path.join(root, "src-tauri/Cargo.lock"), "utf8");
  const bumped = bumpVersion(root, "minor"); assert.notEqual(bumped, old); assert.equal(checkVersions(root), bumped);
  const newLock = fs.readFileSync(path.join(root, "src-tauri/Cargo.lock"), "utf8");
  assert.equal(newLock.replace(`name = "claude-cache-watch"${newline}version = "${bumped}"`, `name = "claude-cache-watch"${newline}version = "${old}"`), oldLock);
  assert.equal(bumpVersion(root, "1.0.0-beta.1"), "1.0.0-beta.1");
  assert.equal(checkVersions(root), "1.0.0-beta.1");
});

test("invalid bumps and version/tag/note mismatches fail without edits", (t) => {
  const root = fixture(t), version = checkVersions(root);
  const before = manifests.map((name) => fs.readFileSync(path.join(root, name), "utf8"));
  for (const target of ["invalid", "v1.0.0", "1.02.3", "0.0.1", "1.0.0-01", "1.0.0;bad"]) assert.throws(() => bumpVersion(root, target));
  assert.deepEqual(manifests.map((name) => fs.readFileSync(path.join(root, name), "utf8")), before);
  assert.equal(checkVersions(root, `v${version}`), version); assert.throws(() => checkVersions(root, "v9.9.9"));
  assert.ok(releaseNotes(root, version).length);
  fs.writeFileSync(path.join(root, "CHANGELOG.md"), `## [${version}]\n\n## [0.0.1]\nOld notes\n`);
  assert.throws(() => checkVersions(root, `v${version}`), /empty/);
});

test("public-data audit finds credentials and compiler paths without echoing values", (t) => {
  const root = fixture(t), token = "sk-ant-" + "x".repeat(30), profile = ["C:", "Users", "example-developer", ".cargo", "file.rs"].join("\\");
  const secret = scanContent(Buffer.from(token), "example.txt"); assert.equal(secret[0].kind, "access token");
  assert.ok(!JSON.stringify(secret).includes(token));
  assert.equal(scanContent(Buffer.from(profile), "file.exe", true)[0].kind, "personal profile path");
  assert.equal(scanContent(Buffer.from(profile, "utf16le"), "file.exe", true)[0].kind, "personal profile path");
  assert.deepEqual(scanContent(Buffer.from(["C:", "Users", "you", ".claude"].join("\\"))), []);
  assert.equal(scanContent(Buffer.from([255]), "invalid.md")[0].kind, "invalid UTF-8");
  const zip = path.join(root, "private.zip"); fs.writeFileSync(zip, zipSync({ "example.exe": Buffer.from(profile) }));
  assert.equal(scanArtifact(zip)[0].kind, "personal profile path");
});

test("release preflight verifies checksums, exact archive members and Windows architecture", (t) => {
  const root = fixture(t), output = packageFixture(root), version = checkVersions(root);
  assert.equal(prepareRelease(root, `v${version}`).assets.length, 2);
  const checksum = path.join(output, `claude-cache-watch-${version}-windows-x64-portable.sha256`);
  fs.appendFileSync(checksum, "tampered"); assert.throws(() => prepareRelease(root), /checksum/);
  packageFixture(root, { "extra.txt": Buffer.from("not a release input") }); assert.throws(() => prepareRelease(root), /unexpected files/);
  packageFixture(root, { "Claude Cache Watch.exe": Buffer.from("not an executable") }); assert.throws(() => prepareRelease(root), /Windows x64/);
});

function context(tag) { return { GITHUB_ACTIONS: "true", GITHUB_REF_TYPE: "tag", GITHUB_REF_NAME: tag, GITHUB_SHA: "1".repeat(40), GITHUB_REPOSITORY: "example/claude-cache-watch", GITHUB_TOKEN: "unit-test-not-a-real-token" }; }
const response = (data, status = 200) => new Response(JSON.stringify(data), { status, headers: { "Content-Type": "application/json" } });
const marker = "<!-- claude-cache-watch-managed-release -->";
function server(release, options = {}) {
  const calls = [], uploads = [];
  let current = options.existing ? structuredClone(options.existing) : null, tagReads = 0, created = 0, published = null;
  const fetchImpl = async (target, request) => {
    const url = new URL(target), method = request.method || "GET";
    calls.push({ path: url.pathname, method });
    assert.equal(request.redirect, "error"); assert.ok(!url.href.includes("unit-test-not-a-real-token"));
    if (url.pathname.includes("/git/ref/")) {
      tagReads++;
      return response({ object: options.annotated ? { type: "tag", sha: "2".repeat(40) } : { type: "commit", sha: options.moved || (options.moveAfterUpload && tagReads > 1) ? "3".repeat(40) : release.sourceCommit } });
    }
    if (url.pathname.includes("/git/tags/")) return response({ object: { type: "commit", sha: release.sourceCommit } });
    if (url.pathname.includes("/releases/tags/")) return current && !current.draft ? response(current) : response({}, 404);
    if (url.pathname.endsWith("/releases") && method === "GET") {
      if (options.paginated && url.searchParams.get("page") === "1") return response(Array.from({ length: 100 }, () => ({ draft: false, tag_name: "other" })));
      return response(current ? [current] : []);
    }
    if (url.pathname.endsWith("/releases") && method === "POST") {
      created++; current = { ...JSON.parse(request.body), id: 1, assets: [], upload_url: "https://uploads.github.com/example/assets{?name,label}" };
      assert.equal(current.draft, true); return response(current, 201);
    }
    if (url.hostname === "uploads.github.com") {
      if (options.failUpload) return response({}, 500);
      const asset = { name: url.searchParams.get("name"), digest: "sha256:" + crypto.createHash("sha256").update(request.body).digest("hex") };
      uploads.push(asset.name); current.assets.push(asset); return response(asset, 201);
    }
    if (method === "GET") return response({ ...current, tag_name: options.retargetAfterUpload ? "v9.9.9" : current.tag_name, assets: options.extraAfterUpload ? [...current.assets, { name: "private-screenshot.png" }] : current.assets });
    assert.equal(method, "PATCH"); assert.equal(current.assets.length, 2);
    published = JSON.parse(request.body); assert.equal(published.draft, false);
    return response({ html_url: "https://github.com/example/claude-cache-watch/releases/tag/" + release.tag });
  };
  return { fetchImpl, calls, uploads, get created() { return created; }, get published() { return published; } };
}
function draft(release, additions = {}) {
  return { id: 1, draft: true, tag_name: release.tag, body: marker, assets: [], upload_url: "https://uploads.github.com/example/assets{?name,label}", ...additions };
}
function releaseFixture(t) { const root = fixture(t); packageFixture(root); return prepareRelease(root); }

test("GitHub publishing keeps the release draft until both assets upload and verifies the tag again", async (t) => {
  const release = releaseFixture(t), api = server(release);
  assert.ok((await publishRelease(release, context(release.tag), api.fetchImpl)).includes(release.tag));
  assert.equal(api.uploads.length, 2); assert.equal(api.created, 1); assert.equal(api.published.make_latest, "legacy");
  assert.equal(api.calls.filter((call) => call.path.includes("/git/ref/")).length, 2);
});

test("failed uploads never publish, and branch/local/dirty contexts never make requests", async (t) => {
  const release = releaseFixture(t);
  const never = () => { throw new Error("must not make network requests"); };
  await assert.rejects(publishRelease(release, {}, never), /matching GitHub Actions tag/);
  await assert.rejects(publishRelease(release, { ...context(release.tag), GITHUB_REF_TYPE: "branch" }, never));
  await assert.rejects(publishRelease({ ...release, sourceDirty: true }, context(release.tag), never), /source commit/);
  await assert.rejects(publishRelease(release, { ...context(release.tag), GITHUB_SHA: "4".repeat(40) }, never), /source commit/);
  const api = server(release, { failUpload: true });
  await assert.rejects(publishRelease(release, context(release.tag), api.fetchImpl), /HTTP 500/); assert.equal(api.published, null);
});

test("published releases are reused only when immutable asset digests match", async (t) => {
  const release = releaseFixture(t);
  for (const matching of [true, false]) {
    const existing = { draft: false, html_url: "https://github.com/example/release", assets: release.assets.map((a) => ({ name: a.name, digest: matching ? `sha256:${a.sha256}` : "sha256:different" })) };
    const api = server(release, { existing });
    if (matching) assert.equal(await publishRelease(release, context(release.tag), api.fetchImpl), existing.html_url);
    else await assert.rejects(publishRelease(release, context(release.tag), api.fetchImpl), /Published release assets differ/);
    assert.ok(api.calls.every((call) => call.method === "GET"));
  }
});

test("paginated draft lookup resumes only managed drafts and reuses verified assets", async (t) => {
  const release = releaseFixture(t), first = release.assets[0];
  const api = server(release, { paginated: true, existing: draft(release, { assets: [{ name: first.name, digest: `sha256:${first.sha256}` }] }) });
  await publishRelease(release, context(release.tag), api.fetchImpl);
  assert.equal(api.created, 0); assert.deepEqual(api.uploads, [release.assets[1].name]); assert.ok(api.published);
  const manual = server(release, { existing: draft(release, { body: "Manually maintained draft" }) });
  await assert.rejects(publishRelease(release, context(release.tag), manual.fetchImpl), /unmanaged draft/);
  assert.equal(manual.created, 0); assert.equal(manual.uploads.length, 0);
});

test("moved tags are rejected and annotated tags resolve to the built commit", async (t) => {
  const release = releaseFixture(t), moved = server(release, { moved: true });
  await assert.rejects(publishRelease(release, context(release.tag), moved.fetchImpl), /tag moved/);
  assert.equal(moved.created, 0);
  const after = server(release, { moveAfterUpload: true });
  await assert.rejects(publishRelease(release, context(release.tag), after.fetchImpl), /tag moved/); assert.equal(after.published, null);
  const annotated = server(release, { annotated: true });
  await publishRelease(release, context(release.tag), annotated.fetchImpl); assert.ok(annotated.published);
});

test("unexpected draft attachments are never published, including additions during upload", async (t) => {
  const release = releaseFixture(t);
  const extra = server(release, { existing: draft(release, { assets: [{ name: "private-screenshot.png" }] }) });
  await assert.rejects(publishRelease(release, context(release.tag), extra.fetchImpl), /unexpected/); assert.equal(extra.published, null); assert.equal(extra.uploads.length, 0);
  const race = server(release, { extraAfterUpload: true });
  await assert.rejects(publishRelease(release, context(release.tag), race.fetchImpl), /unexpected/); assert.equal(race.published, null);
});

test("a draft retargeted during upload is not published", async (t) => {
  const release = releaseFixture(t), api = server(release, { retargetAfterUpload: true });
  await assert.rejects(publishRelease(release, context(release.tag), api.fetchImpl), /Draft changed/);
  assert.equal(api.published, null);
});

test("prerelease packages are not marked as the latest stable release", async (t) => {
  const root = fixture(t), version = bumpVersion(root, "1.0.0-beta.1");
  fs.writeFileSync(path.join(root, "CHANGELOG.md"), `## [${version}]\n\n- Preview build.\n`);
  packageFixture(root); const release = prepareRelease(root), api = server(release);
  assert.equal(release.prerelease, true);
  await publishRelease(release, context(release.tag), api.fetchImpl); assert.equal(api.published.make_latest, "false");
});
