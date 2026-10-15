# Claude Cache Watch

A portable Windows tray app that watches local Claude Code transcripts, alerts on expensive uncached calls, and keeps searchable SQLite history. Built with Tauri 2, Rust, and a small dependency-free web interface.

## Run

Launch `Claude Cache Watch.exe` from the release folder. Close the window to continue monitoring in the system tray. Use **Open history** or **Quit monitor** in the tray menu. Launching a second copy opens the existing window.

For a hidden launch, run `"Claude Cache Watch.exe" --background`. To start at sign-in, put a shortcut to that command in your Windows Startup folder (`shell:startup`). The app does not change startup settings automatically.

The EXE needs no installer. Windows must have the Microsoft Edge WebView2 runtime (normally present on Windows 10/11). There is no bundled WebView2 runtime. Move the EXE wherever you like; history remains in `%LOCALAPPDATA%\local.claude-cache-watch\history.sqlite3`.

## Alert semantics

The default condition is **ordinary input tokens + cache-creation input tokens > 100,000** for one API request. Cache reads and output tokens do not count. Cache writes are uncached work, even though they populate the cache for subsequent calls. Exactly 100,000 does not alert.

Enable **Only when cache reads = 0** to ignore partially cached calls. The threshold is configurable. Settings apply to newly processed usage; changing them does not reclassify old history.

The app checks approximately every 3 seconds, plus time spent processing logs. Alerts can only fire after Claude Code writes usage into its transcript, usually when a response arrives; this does not intercept or cancel in-flight API calls. Both main sessions and nested subagent transcripts are included. Only calls with usage and a request or message ID are recorded. Repeated streaming/transcript entries merge into one row. Counters retain their highest observed values.

Existing file content is imported **without alerts** at each app launch and whenever the source folder changes. Calls logged while the app was closed are therefore recorded silently. Once running, newly appended calls and newly discovered session/subagent files can alert. Pause stops ingestion and alerts; resuming processes accumulated entries under the current rule. A first import can take time with large logs, with bounded per-file batches so active sessions can continue to advance.

The default source is `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` when that environment variable is set. You can change it in the window. Missing or unreadable folders/files appear as monitor errors; they are retried on subsequent scans. File positions survive restarts. Truncated files or replacements with a different initial prefix are replayed and deduplicated. Replacements that preserve the first 256 bytes and are at least as long as the saved offset cannot be distinguished from appends.

## History and notifications

History contains timestamps, request/session IDs, project paths, model names, token counters, and notification outcomes. **Prompts, replies, tool arguments, credentials, and API keys are not saved.** Everything stays on this computer; the app has no network client or telemetry. History has no automatic retention limit. Search by project, model, request, or session; filter alerts and export the current 100-row page as JSON into your Downloads folder under `Claude Cache Watch` (the saved path appears in the window). **Open data folder** opens the database location. Back up SQLite only after quitting, or use SQLite's backup API (WAL mode).

Notifications use Windows WinRT with the built-in PowerShell notification identity to support a portable executable. They can appear under Windows PowerShell in Windows notification settings. **Test desktop alert** verifies submission. **Submitted** means Windows accepted the toast, not that it displayed a banner: Focus/Do Not Disturb and Windows notification settings control presentation. Failed submissions are marked **failed**, with the error on hover; they are not retried automatically. Alerts are persisted before delivery. A crash between submitting a toast and saving its outcome can repeat that toast on restart (at-least-once delivery).

## Session titles and estimated API billing

Each call shows its parent session title, plus the subagent description when applicable. Custom session names take precedence over generated AI titles, legacy summaries, and slugs. Titles are refreshed from transcript metadata; subagent descriptions come from the corresponding `agent-*.meta.json` sidecar. If a title was not recorded, the session or subagent ID is shown. Search and JSON export include both titles. Only these title fields are retained from session metadata; full prompts and tool arguments are not stored.

**Est. API USD** shows the estimated cost of each call. Expand an amount to see input, cache-write, cache-read, output, and recorded web-search charges, plus the rates and assumptions used. The page subtotal covers the currently displayed rows and separately counts calls needing pricing. Estimates use [Anthropic's published API pricing](https://platform.claude.com/docs/en/about-claude/pricing), verified **2026-10-16**, rather than attempting to reconstruct historical price changes.

Input, 5-minute cache writes, 1-hour cache writes, cache reads and output are priced separately. Recorded fast mode and US inference modifiers are applied. Haiku 5.5 switches to higher rates when total input (including cache writes and reads) exceeds 100,000 tokens. Missing cache duration assumes 5 minutes; missing speed/geography assumes standard/global, with assumptions shown in the breakdown. The transcript geography marker `not_available` also uses the explicit global-pricing assumption. Other unsupported speed/geography values require custom rates. Recorded web searches add $0.01 each. Tax, other server-tool charges, negotiated discounts, and subscription fees are excluded. Claude subscription usage is not a separate API invoice: this is an equivalent API cost estimate.

Under **API billing rates**, select or enter an exact model ID to override its five rates in USD per million tokens. Custom rates are final effective rates: they replace the model's tiers and fast/region modifiers, so include those premiums yourself. Overrides apply to existing history and exports immediately. **Use published rates** removes an override; unknown models remain **Pricing needed**, never silently priced as another model or as zero. Unknown fast-mode pricing also needs custom rates.

On the first upgrade, the database migration preserves calls, alerts and settings, and resets file cursors once to refill billing metadata and titles. This replay uses the existing silent startup-import behavior. Missing original transcripts cannot be backfilled; duration/speed/geography assumptions remain visible for those rows. No historical alerts are intentionally replayed.

## Statistics interval

The homepage shows call counts and rate, estimated API spend and pricing coverage, input/output and uncached tokens, cache reads/writes and efficiency, cache-miss and high-uncached-call counts, alert outcomes, session/subagent/project counts, average/maximum call cost, and a breakdown by model. Costs only sum priced calls; unknown pricing is counted separately. The cache-read discount compares the read price against fresh input for those tokens; it does not subtract cache-write overhead or represent net cache savings.

The two-handle slider runs from **Now on the far left** toward older history on the right, with a nonlinear scale that provides more precision near now. When the recent/left handle is at zero, statistics follow now and refresh every three seconds. Moving it right freezes both time boundaries for historical review; changes to either handle recalculate the cards. Presets select the last hour, 24 hours, seven days, or all history and return to live mode. The time interval applies to cards and the model breakdown; the paginated call-history search remains independent. Records without valid timestamps remain in history but are excluded from interval statistics and counted in the interval status.

## Develop / build

Requires Node.js, Rust with the MSVC toolchain, Visual Studio C++ build tools + Windows SDK, and WebView2.

```powershell
npm install
npm run dev
npm test
npm run check
npm run build
```

The portable binary is `src-tauri/target/release/claude-cache-watch.exe`; `npm run build` deliberately disables installer bundling. `Cargo.lock` and `package-lock.json` pin the resolved dependencies. Tests use synthetic transcripts and isolated temporary databases, without modifying real Claude logs.
