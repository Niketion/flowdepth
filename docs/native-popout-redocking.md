# Native popout redocking

On Windows, drag a detached pane by its native title bar or pane header. When the pointer enters an eligible dashboard, a guide appears in the center and shows the positions with enough room for the incoming pane. Move over a position to highlight it and preview the pane's exact placement. Release on that position to dock the pane.

The guide reads **Merge into this window** over the dashboard of another running Flowdepth process that supports this feature. The receiving process loads the pane's saved configuration and rebuilds its data using its own profile. The source closes the popout after the receiving process acknowledges the merge. Each process continues saving its own layouts, so use separate data paths to keep independently running profiles' saved layouts separate.

If the source cannot confirm a merge, retry on the same running destination to recover the original result. If the source pane's configuration changed while a merge was pending, the source keeps the edited popout open and shows a notice. Both instances need builds that support the same transfer protocol.

You can also hold the pointer briefly near a dashboard edge. Flowdepth recommends the closest available side and shows its placement preview. Release near that edge while the preview is visible to dock or merge there. When two sides are equally close near a corner, the guide stays neutral.

Release outside an active tile or edge suggestion to keep the popup detached, or press Escape to cancel the gesture. Restore a maximized pane before docking into the grid. The **Merge** action is available on all supported desktop platforms, including Linux and macOS, which use it to return detached panes.

For a saved popout containing multiple panes, use each pane's **Merge** action. This returns the selected pane and leaves its siblings in the popup.

## Manual desktop test matrix

Use the feature branch build with an isolated `FLOWSURFACE_DATA_PATH`. Record the Windows version, monitor arrangement, display scaling, and Flowdepth UI scale with the results.

| Scenario | Expected result |
| --- | --- |
| Enter the dashboard, then release in the interior outside the guide or between its positions | The guide appears on entry. The popup stays detached. |
| Hover over each position and release while it is highlighted | The preview indicates the new pane's region. The pane joins the corresponding edge and only its popup closes. |
| Move between positions, then into the dashboard interior away from the guide and edges | The placement preview follows the highlighted position and clears in the neutral interior. The guide disappears outside the grid. |
| Sweep past an edge quickly, then hold the pointer near an edge away from the guide | The quick pass keeps the guide neutral. Holding near the edge shows its placement preview and enables release there. |
| Hold the pointer near a corner at nearly equal distances from both available sides | The guide stays neutral. Moving closer to one side allows that side to be suggested. |
| Show a preview, then press Escape, cancel capture, switch applications, or release outside the target | Preview clears and the popup remains independent. |
| Resize a popup, use the window system menu to move it, or snap it to another part of the desktop | Native window behavior works without docking. |
| Move popups between mixed-DPI monitors with negative desktop coordinates | The pointer and preview line up on the receiving monitor. Repeat with a non-default Flowdepth UI scale. |
| Maximize or minimize either window, and maximize a pane in the main dashboard | No docking while the receiving window is minimized, the source is maximized or minimized, or a pane is maximized. A maximized receiving window remains a valid target. |
| Detach two panes, dock one, then use Merge on the other | Only the requested popup closes each time. Charts, indicators, links, and active feeds follow the panes. |
| Run two builds with separate data paths and drag a popout onto a position in the other dashboard's guide | The guide identifies the operation as a merge. Release transfers the configuration, closes the source popout, and initializes data in the receiving dashboard. |
| Close or change the destination layout before a cross-process drop is accepted | The source popup remains available when the receiving process rejects the transfer. |
| Shrink the destination before it handles a pending offer so the chosen split no longer fits | The destination rejects the offer and the source popout remains open. |
| Lose the acknowledgement after import, then retry on the same running destination | The destination acknowledges the earlier import without creating a second pane. |
| Change the popout configuration while a peer merge is pending | The destination gets the exported configuration. The edited source popout stays open with a notice. |
| Switch layouts with detached panes, close a popup, and save and reopen after docking | Layouts restore their own panes. A docked pane appears once in the main grid and has no saved popup entry. |
| Use Merge on Linux or macOS | The pane returns to the main grid and the source window closes. |
