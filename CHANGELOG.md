# Changelog

Versions follow Semantic Versioning. Release tags use `vMAJOR.MINOR.PATCH`, with optional prerelease identifiers.

## [Unreleased]

## [0.2.0] - 2026-11-09

- Add read-only Antigravity IDE/CLI adapters, subagents, compaction and terminal usage snapshots with WAL support and transcript fallback.
- Share source/call-type filters across searchable history, statistics, charts and JSON exports; show unknown usage explicitly.
- Preserve existing history, alerts, settings, cursors and Claude rates during transactional migration.
- Add source-scoped reference API pricing with explicit missing dimensions and no inferred internal model aliases.
- Detect transcript rewrites beyond the initial prefix and retry independent sources after locks/errors.
- Add native-format, deduplication, migration and interface regression tests plus an aggregate local verification command.

## [0.1.0] - 2026-10-16

- Monitor Claude Code calls in the Windows system tray and alert on high uncached input usage.
- Keep searchable local history with session and subagent titles and JSON exports.
- Estimate API billing with cache-duration pricing and configurable model rates.
- Explore live and historical statistics with metric trend charts and card sparklines.
- Build portable Windows x64 packages with privacy checks, synchronized versions, and GitHub Actions releases.
