# Claude Cache Watch {{VERSION}}

Extract this archive, then run **Claude Cache Watch.exe**. Windows 10/11 x64 and Microsoft Edge WebView2 are required. No installer, API key, or account connection is needed.

The app watches local Claude Code transcripts and Antigravity IDE/CLI session databases, including subagents and context compaction. Existing default Antigravity folders are enabled automatically. Confirm enabled source folders in the window, then use **Test desktop alert**. Closing the window keeps monitoring in the system tray; **Open history** opens it again and **Quit monitor** stops it.

By default, a desktop alert is submitted when ordinary input plus cache-creation input tokens exceeds 100,000. Cache reads and output do not count toward that threshold. Existing logs are imported silently. Windows notification settings and Focus/Do Not Disturb control whether a banner appears.

The dashboard shows searchable call history, session/subagent titles, API cost estimates, and metric trends. The recent interval handle at the far left enables live updates. API costs are reference estimates, not actual Antigravity/Claude subscription deductions; unknown model aliases remain unpriced. Missing usage remains unknown. Source and call-type filters also apply to charts and JSON exports. Published price sources, verification dates and excluded billing dimensions are shown in each cost breakdown. Estimated API costs are not invoices or additional subscription charges; assumptions and custom rates are available in the app.

History and settings are stored in `%LOCALAPPDATA%\local.claude-cache-watch`. The app does not upload data or store full prompts/replies. Project paths, titles and usage totals can still be private: review screenshots and exports before sharing.

Use the source repository's README and documentation for development, pricing details, and release updates. The adjacent changelog describes this version. The separately published `.sha256` file verifies the downloaded ZIP.

Licensed under the MIT License. Copyright (c) 2026 theTd. See the included LICENSE file for terms.
