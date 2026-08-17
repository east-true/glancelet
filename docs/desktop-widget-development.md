# Desktop Widget Experience development

Phase 6 turns the original Today/Inbox proof of concept into one persistent Desktop Surface with four built-in Widgets: Today, Inbox, Upcoming, and Attention. Phase 7 adds the daily-use loop: a built-in global show/hide shortcut, local Quick Capture, and presentation-layer Privacy Mode.

The built-in Surface is light-first: a warm neutral desktop background, white Widgets, subtle borders, restrained semantic indicators, and muted Source provenance keep Work titles at the top of the visual hierarchy. Layout controls stay hidden outside Edit Layout mode; this is a concrete product style, not a Theme or external Widget SDK contract.

Phase 6 finalization visually reviewed the real React components in default, compact, many-item, empty, Upcoming, focused-action, and Edit Layout states, followed by a window-only capture of the running Tauri/WebKit surface. Work hierarchy, density, compact-window behavior, focus actions, and restrained Attention styling passed; the temporary fixtures and captures are not repository artifacts.

## Persistence boundaries

- `widget_instances` stores built-in Widget order, discrete size, and small settings JSON.
- `desktop_preferences` stores Always on Top, global-shortcut enablement, and Privacy Mode. Privacy Mode remains enabled across restart so a deliberately hidden Surface does not reveal content on next launch.
- Tauri's official window-state plugin stores native window position and size and Glancelet recovers geometry that no longer intersects an available monitor.
- The official autostart plugin owns the operating-system login registration. Autostart is off until the user enables it.

No credential or provider payload belongs in any presentation setting. Widgets receive only application-owned WorkView read models.

Quick Capture bootstraps one stable, credential-free `local.manual` SourceConfig and passes a normalized local SourceRecord through the existing SourceChange and Capture projector path. It does not create a manual-task table or a second Work persistence path. Privacy Mode is presentation redaction, not encryption: Work titles remain in the local database, but title, summary, Source display data, dimensions, and facets are removed before a WorkView is serialized to the frontend.

The default global shortcut is `CommandOrControl+Shift+Space`. Registration uses Tauri 2's official [Global Shortcut plugin](https://v2.tauri.app/plugin/global-shortcut/) in the Rust runtime, so no frontend shortcut permission is exposed. A conflict does not prevent startup; Settings reports that the shortcut is unavailable while the Surface and tray remain usable.

## Manual E2E checklist

1. Launch Glancelet and confirm the Desktop Surface opens first.
2. Confirm cached Today and Inbox work appears before provider synchronization completes.
3. Check Today, Inbox, and Attention empty states with no matching work.
4. With no configured sources, confirm both **Capture something** and **Connect a Source** are offered.
5. Enter **Edit Layout** and add Upcoming.
6. Remove a Widget while leaving at least one Widget on the Surface.
7. Reorder Widgets by drag and drop and by the accessible arrow controls.
8. Cycle a Widget through compact, wide, and tall sizes.
9. Restart Glancelet and confirm Widget order and sizes are restored.
10. Move and resize the window, restart, and confirm geometry is restored.
11. Disconnect the previous monitor or reduce resolution and confirm the window recovers on screen.
12. Toggle **Always on Top**, restart, and confirm the setting persists.
13. Enable **Launch Glancelet at startup**, sign out/in in a disposable test environment, and then disable it.
14. Close the window and confirm the process remains available in the tray.
15. Use tray **Show Glancelet** and **Hide Glancelet**.
16. Use tray **Sources** and **Settings** and confirm the window focuses on the requested section.
17. Use tray **Quit** and confirm the process exits.
18. In Inbox, plan an Action for Today, Tomorrow, and Backlog.
19. Snooze, dismiss, pin, and unpin Work and confirm Widgets refresh after command success.
20. Confirm external-progress and no-progress Work never exposes local Start or Complete actions.
21. Open Slack, Notion, Google, GitHub, and GitLab Work and confirm navigation reaches the original HTTPS resource.
22. Put one source into AuthenticationRequired and confirm cached Work remains visible with a non-blocking source-health warning.
23. Hide Glancelet, press `CommandOrControl+Shift+Space`, and confirm the existing window shows, unminimizes, and focuses; press it again to hide.
24. Disable and re-enable **Global shortcut** in Settings. If the shortcut is already claimed, confirm the warning is non-blocking and tray show/hide still works.
25. Press `C` on the Surface, type a title, and press Enter. Confirm the input focuses immediately and the captured Action appears in Inbox without provider sync.
26. Repeat Quick Capture with Today, Tomorrow, and Backlog planning. Confirm each uses the existing Widget semantics and a double submit creates only one Work.
27. Restart and confirm local captures remain available without connecting an external Source.
28. Turn **Privacy Mode** on from the Surface and tray. Confirm titles and Source details become generic while time, planning, and allowed actions remain useful.
29. Restart while Privacy Mode is on and confirm the first dashboard payload is still redacted; turn it off and confirm normal WorkViews return.
30. With Privacy Mode on, create a Quick Capture and confirm its title is visible only while typing, then redacted in the Widgets.

Linux tray icon click events are not emitted by the underlying Tauri tray implementation; the context menu remains available and is the supported show/hide path on Linux.
