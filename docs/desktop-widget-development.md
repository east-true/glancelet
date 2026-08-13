# Desktop Widget Experience development

Phase 6 turns the original Today/Inbox proof of concept into one persistent Desktop Surface with four built-in Widgets: Today, Inbox, Upcoming, and Attention.

The built-in Surface is light-first: a warm neutral desktop background, white Widgets, subtle borders, restrained semantic indicators, and muted Source provenance keep Work titles at the top of the visual hierarchy. Layout controls stay hidden outside Edit Layout mode; this is a concrete product style, not a Theme or external Widget SDK contract.

Phase 6 finalization visually reviewed the real React components in default, compact, many-item, empty, Upcoming, focused-action, and Edit Layout states, followed by a window-only capture of the running Tauri/WebKit surface. Work hierarchy, density, compact-window behavior, focus actions, and restrained Attention styling passed; the temporary fixtures and captures are not repository artifacts.

## Persistence boundaries

- `widget_instances` stores built-in Widget order, discrete size, and small settings JSON.
- `desktop_preferences` stores the Always on Top preference.
- Tauri's official window-state plugin stores native window position and size and Glancelet recovers geometry that no longer intersects an available monitor.
- The official autostart plugin owns the operating-system login registration. Autostart is off until the user enables it.

No credential or provider payload belongs in any presentation setting. Widgets receive only application-owned WorkView read models.

## Manual E2E checklist

1. Launch Glancelet and confirm the Desktop Surface opens first.
2. Confirm cached Today and Inbox work appears before provider synchronization completes.
3. Check Today, Inbox, and Attention empty states with no matching work.
4. With no configured sources, use **Connect a Source** and confirm it opens Sources.
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

Linux tray icon click events are not emitted by the underlying Tauri tray implementation; the context menu remains available and is the supported show/hide path on Linux.
