# Slicer XDND compatibility patch

Based on gpui-pre-linux 0.3.5 (Apache-2.0; see LICENSE-APACHE).

The X11 backend now requests text/uri-list regardless of advertised format order,
resets state on enter, submits only after asynchronous file data arrives, ignores
unrelated selection notifications, and converts pointer coordinates to logical
pixels. This fixes file-manager drops that were silently parsed as empty text.

Reproduce with examples/xdnd_check.rs: mixed, delayed, and type-list options.

Window move/resize requests identify the initiating left mouse button as X11
button 1 (rather than 0) and the source as a normal application. This lets the
window manager reliably track the held button through an interactive resize.
