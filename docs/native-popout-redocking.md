# Native popout redocking

On Windows, drag a detached pane by its native title bar. A small marker appears when the pointer enters an eligible edge region of the main pane grid. Hold the pointer in the marker until it expands into a translucent placement preview, then release. The pane joins the corresponding edge of the grid.

Release away from a preview to place the popup independently, or press Escape to cancel the gesture. The four activation regions are near the midpoints of the pane-grid edges. A split must have enough room for the incoming pane and the existing grid. Restore a maximized pane before docking into the grid. The **Merge** action is available on all supported desktop platforms, including Linux and macOS, which use it to return detached panes.

For a saved popout containing multiple panes, use each pane's **Merge** action. This returns the selected pane and leaves its siblings in the popup.

## Manual desktop test matrix

Use the feature branch build with an isolated `FLOWSURFACE_DATA_PATH`. Record the Windows version, monitor arrangement, display scaling, and Flowdepth UI scale with the results.

| Scenario | Expected result |
| --- | --- |
| Drag across the dashboard, release in the interior, or overlap it with the pointer outside | Popup stays independent. Crossing a marker quickly never docks. |
| Enter each edge marker, hold until its placement preview appears, then release | Preview indicates the new pane's region. The pane joins the matching edge and only its popup closes. |
| Enter an edge, then move to another edge or leave the grid | Placement changes only after the new target arms. Leaving clears the preview. |
| Arm a preview, then press Escape, cancel capture, switch applications, or release outside the target | Preview clears and the popup remains independent. |
| Resize a popup, use the window system menu to move it, or snap it to another part of the desktop | Native window behavior works without docking. |
| Move popups between mixed-DPI monitors with negative desktop coordinates | The pointer and preview line up on the receiving monitor. Repeat with a non-default Flowdepth UI scale. |
| Maximize or minimize either window, and maximize a pane in the main dashboard | No docking while the receiving window is minimized, the source is maximized or minimized, or a pane is maximized. A maximized receiving window remains a valid target. |
| Detach two panes, dock one, then use Merge on the other | Only the requested popup closes each time. Charts, indicators, links, and active feeds follow the panes. |
| Switch layouts with detached panes, close a popup, and save and reopen after docking | Layouts restore their own panes. A docked pane appears once in the main grid and has no saved popup entry. |
| Use Merge on Linux or macOS | The pane returns to the main grid and the source window closes. |
