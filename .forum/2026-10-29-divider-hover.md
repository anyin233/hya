# How should pane divider hover cues match drag behavior?

Hover and dragging must use the same hit test, including the boundary cell and preceding cell, innermost-container precedence at intersections, and indices of the actual visible siblings in the saved tree. state/paneResize.ts owns ResizeBoundary, resizeBoundaryAt and resizeGuide. PaneWorkspace memoizes geometry and stores hover/drag target keys; pointer motion on the same target does not update the durable layout or remount pane views.

The one-cell unboxed guide overlays the existing divider: horizontal lines with ↕ resize heights; vertical lines with ↔ resize widths. It spans only the affected container and stays on the live divider during drag. Use the neutral border token, preserving the keyboard focus color. Save weights only on release; overlay or terminal blur cancels an unfinished drag. Gate hover on terminal focus so native hit-grid refresh cannot resurrect a cleared guide after blur.

# Can a TUI change the resize pointer shape?

OpenTUI renderer.setMousePointer emits OSC22 and supports ew-resize/ns-resize. Request them best-effort and reset to the protocol-defined default, not auto (OpenTUI otherwise emits literal auto). Terminal implementations vary, so the Unicode cue remains usable independently of the pointer shape. Primary protocol: https://sw.kovidgoyal.net/kitty/pointer-shapes/ .

# Why did the PTY tests previously miss hover behavior?

The generic PTY Tui.mouse.move implementation only sent button-held motion. It now sends button3/no-button SGR motion (35) in any-motion mode; drag-only mode suppresses no-button movement. Tests capture real OSC22 requests and assert buffer glyphs, focus, unchanged layout files, live dragging, and blur cancellation at wide/narrow sizes. A drag test that waits for the live frame must separately wait for first preferences-file creation after mouse-up before parsing it.
