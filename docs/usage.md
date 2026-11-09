# Detailed usage

A portable Windows tray app that watches Claude Code transcripts and Antigravity IDE/CLI databases, alerts on expensive uncached calls, and keeps searchable SQLite history. Built with Tauri 2, Rust, and a small dependency-free web interface.

## Run

Launch `Claude Cache Watch.exe` from the release folder. Close the window to continue monitoring in the system tray. Use **Open history** or **Quit monitor** in the tray menu. Launching a second copy opens the existing window.

For a hidden launch, run `"Claude Cache Watch.exe" --background`. To start at sign-in, put a shortcut to that command in your Windows Startup folder (`shell:startup`). The app does not change startup settings automatically.

The EXE needs no installer. Windows must have the Microsoft Edge WebView2 runtime (normally present on Windows 10/11). There is no bundled WebView2 runtime. Move the EXE wherever you like; history remains in `%LOCALAPPDATA%\local.claude-cache-watch\history.sqlite3`.

## Alert semantics

The default condition is **ordinary input tokens + cache-creation input tokens > 100,000** for one API request. Cache reads and output tokens do not count. Cache writes are uncached work, even though they populate the cache for subsequent calls. Exactly 100,000 does not alert.

Enable **Only when cache reads = 0** to ignore partially cached calls. The threshold is configurable. Settings apply to newly processed usage; changing them does not reclassify old history.

The app checks approximately every 3 seconds, plus time spent processing logs. Unchanged transcripts and subagent title sidecars are checked by size and modification time, without reopening their contents or querying history. Dashboard history and statistics refresh only while the window is visible and not minimized; closing to the tray keeps log monitoring and alerts active. Reopening refreshes the dashboard within the next three-second check. Alerts can only fire after Claude Code writes usage into its transcript, usually when a response arrives; this does not intercept or cancel in-flight API calls. Both main sessions and nested subagent transcripts are included. Only calls with usage and a request or message ID are recorded. Repeated streaming/transcript entries merge into one row. Counters retain their highest observed values.

Existing file content is imported **without alerts** at each app launch and whenever the source folder changes. Calls logged while the app was closed are therefore recorded silently. Once running, newly appended calls and newly discovered session/subagent files can alert. Pause stops ingestion and alerts; resuming processes accumulated entries under the current rule. A first import can take time with large logs, with bounded per-file batches so active sessions can continue to advance.

The default source is `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` when that environment variable is set. You can change it in the window. Missing or unreadable folders/files appear as monitor errors; they are retried on subsequent scans. File positions survive restarts. Changed Claude transcripts are checked against a persisted fingerprint of the processed bytes; truncation, replacement, same-length rewrites and edits beyond the original prefix are replayed and deduplicated. Antigravity checks DB/WAL and transcript timestamps and lengths, and restarts unfinished batches when the file changes. Files whose metadata remains unchanged are skipped; modifications deliberately preserving both length and filesystem timestamps are outside this detection contract.

## Antigravity sources

Default roots are `~/.gemini/antigravity` (IDE/app) and `~/.gemini/antigravity-cli`. Each has an enable switch and editable path. Existing default directories are enabled when upgrading legacy settings; saved thresholds, pause state and custom Claude paths survive. A source error does not stop the other sources. New installations using `~/.gemini/antigravity-ide` can point the IDE source there.

The adapter reads `conversations/<id>.db` with SQLite read-only/WAL connections and a short busy timeout. Locks retry next poll. Step metadata supplies usage; generation metadata supplies only explicitly linked models. Generation totals are never added to step totals. Final snapshots replace counters, so later corrections can reduce them. Response and checkpoint/compaction calls are separate types and both are included by default. Completed, cleared, cancelled, error and interrupted steps with recorded usage are retained. Pending/running/generating/waiting steps are excluded. Terminal model steps with no usage remain incomplete, with unknown cache counters and no alert.

When a session database is absent, only `.system_generated/logs/transcript.jsonl` is read; `transcript_full.jsonl` is excluded. Source instance + raw session + step index is the identity. A later native database enriches the same rows. Summary databases and explicit subagent descriptors supply titles, workspace and parent relationships; child calls are counted once, never added again to a parent total. Each call exports its source, raw session, parent, type, completion state, usage completeness and metadata source.

Only new, sufficiently complete terminal usage qualifies for an alert. Startup history, cleared steps, rescans, fallback promotion and metadata enrichment do not create new notifications. Missing JSON cache fields remain unknown; omitted scalars in the recognized native protobuf usage schema have its default zero value. Incomplete first observations remain silent if later enriched. A source's call timestamp establishes the startup cutoff.

**Source** and **Call type** filters are shared by history, statistics, trends and the current-page JSON export. Search and history pagination remain independent of the statistics time interval.

## History and notifications

History contains timestamps, request/session IDs, project paths, model names, token counters, and notification outcomes. **Prompts, replies, tool arguments, credentials, and API keys are not saved.** Everything stays on this computer; the app has no network client or telemetry. History has no automatic retention limit. Search by project, model, request, or session; filter alerts and export the current 100-row page as JSON into your Downloads folder under `Claude Cache Watch` (the saved path appears in the window). **Open data folder** opens the database location. Back up SQLite only after quitting, or use SQLite's backup API (WAL mode).

Notifications use Windows WinRT with the built-in PowerShell notification identity to support a portable executable. They can appear under Windows PowerShell in Windows notification settings. **Test desktop alert** verifies submission. **Submitted** means Windows accepted the toast, not that it displayed a banner: Focus/Do Not Disturb and Windows notification settings control presentation. Failed submissions are marked **failed**, with the error on hover; they are not retried automatically. Alerts are persisted before delivery. A crash between submitting a toast and saving its outcome can repeat that toast on restart (at-least-once delivery).

## Session titles and estimated API billing

Each call shows its parent session title, plus the subagent description when applicable. Custom session names take precedence over generated AI titles, legacy summaries, and slugs. Titles are refreshed from transcript metadata; subagent descriptions come from the corresponding `agent-*.meta.json` sidecar. If a title was not recorded, the session or subagent ID is shown. Search and JSON export include both titles. Only these title fields are retained from session metadata; full prompts and tool arguments are not stored.

**Est. API USD** shows the estimated cost of each call. Expand an amount to see input, cache-write, cache-read, output, and recorded web-search charges, plus the rates and assumptions used. The page subtotal covers the currently displayed rows and separately counts calls needing pricing. Estimates use [Anthropic's published API pricing](https://platform.claude.com/docs/en/about-claude/pricing), verified **2026-10-16**, rather than attempting to reconstruct historical price changes.

Input, 5-minute cache writes, 1-hour cache writes, cache reads and output are priced separately. Recorded fast mode and US inference modifiers are applied. Haiku 5.5 switches to higher rates when total input (including cache writes and reads) exceeds 100,000 tokens. Missing cache duration assumes 5 minutes; missing speed/geography assumes standard/global, with assumptions shown in the breakdown. The transcript geography marker `not_available` also uses the explicit global-pricing assumption. Other unsupported speed/geography values require custom rates. Recorded web searches add $0.01 each. Tax, other server-tool charges, negotiated discounts, and subscription fees are excluded. Claude subscription usage is not a separate API invoice: this is an equivalent API cost estimate.

Under **API billing rates**, select or enter an exact model ID to override its five rates in USD per million tokens. Custom rates are final effective rates: they replace the model's tiers and fast/region modifiers, so include those premiums yourself. Overrides apply to existing history and exports immediately. **Use published rates** removes an override; unknown models remain **Pricing needed**, never silently priced as another model or as zero. Unknown fast-mode pricing also needs custom rates.

Database migration is transactional and preserves calls, alert outcomes, settings, custom Claude rates and file cursors. Existing calls are classified as Claude Code. Antigravity identities are namespaced by source instance; IDE and CLI share only their reference-pricing family. No historical alerts are intentionally replayed.

## Antigravity reference prices

Reference API estimates are **not actual subscription deductions or quota consumption**. Exact supported model IDs use [Google Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing) or [Anthropic API pricing](https://platform.claude.com/docs/en/about-claude/pricing), verified 2026-11-09. Internal IDs such as `gemini-pro-default` and numeric placeholders are not guessed. Unknown models or missing dimensions are marked unpriced/unknown.

Gemini 4 Argon standard rates per million tokens are input $2 / cache read $0.10 / output $10 before 2026-12-01, then $4 / $0.20 / $20. Gemini 3.6/3.7/3.8 Flash introductory rates are $0.75 / $0.075 / $3.75 through 2026-12-31, then $1.50 / $0.15 / $7.50. Gemini 2.5 Pro uses $1.25 / $0.125 / $10 up to 200k prompt tokens, then $2.50 / $0.25 / $15. Dates use the recorded call time; these are a verified price snapshot, not an exhaustive historical tariff database.

Output already includes thinking and is charged once. Antigravity does not report cache TTL or all storage/tool/region/service-tier billing dimensions; these are excluded and noted. JSON fallback cache-write usage and unreported web-search costs export `null` amounts. Native cache-write usage can be zero, but positive writes without a verified TTL remain unpriced unless explicit custom effective rates are supplied. Under API billing rates, choose the source family: Antigravity IDE and CLI share overrides, while old Claude overrides are preserved separately.

For aggregate read-only verification without running the tray app, run `"Claude Cache Watch.exe" --verify-antigravity --report audit.json`. The report compares native counters with independent transcript counters, imported records and statistics, including incomplete/undated records and historical alert counts. It contains aggregate data only. `CACHE_WATCH_DATA_DIR` can isolate a manual smoke-test profile; the default profile is unchanged.

## Statistics interval

The trend chart supports all dashboard values, including per-call averages/peaks, pricing coverage, cache efficiency, alert outcomes, and active sessions/subagents/projects/models. Select a metric or click a card's small trend line. Hover the main chart, or focus it and use Left/Right/Home/End, to inspect exact bucket times, values and pricing coverage. Charts follow the same live/fixed interval as the cards and run from older to newer left-to-right.

The app chooses at most 60 time buckets for the selected interval, fills inactive periods with zero counts, and calculates totals and bucket values in the same database pass and snapshot. Counts and token/cost sums are per bucket, not cumulative. Active session/subagent/project/model counts are distinct within each bucket. Ratios use the bucket's own denominators; averages use qualifying calls. Unknown prices produce gaps when a nonempty bucket has no priced calls, and partial pricing remains identified in the inspection details. Alert outcomes are attributed to the original call time, since separate notification timestamps are not recorded.

The homepage shows call counts and rate, estimated API spend and pricing coverage, input/output and uncached tokens, cache reads/writes and efficiency, cache-miss and high-uncached-call counts, alert outcomes, session/subagent/project counts, average/maximum call cost, and a breakdown by model. Costs only sum priced calls; unknown pricing is counted separately. The cache-read discount compares the read price against fresh input for those tokens; it does not subtract cache-write overhead or represent net cache savings.

The two-handle slider runs from **Now on the far left** toward older history on the right, with a nonlinear scale that provides more precision near now. When the recent/left handle is at zero, statistics follow now and refresh every three seconds. Moving it right freezes both time boundaries for historical review; changes to either handle recalculate the cards. Presets select the last hour, 24 hours, seven days, or all history and return to live mode. The time interval applies to cards and the model breakdown; the paginated call-history search remains independent. Records without valid timestamps remain in history but are excluded from interval statistics and counted in the interval status.

## Development

See [development and releases](releasing.md) for build prerequisites, version management, and GitHub Actions.
