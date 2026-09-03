# Look & Feel

The interface is almost entirely black, white, and grey. Red is reserved for
errors: status lines, error toasts, and the problems count. Focused panel
headers are inverted. Hovered and selected items use a grey background.

## Type

Body text is monospace at about 14 px. Panel content and controls use bundled
Geist Mono on every platform. The shell uses Menlo on macOS for panel headers,
tabs, and a few status messages; elsewhere it falls back to bundled Liberation
Mono. Titles, table headers, and buttons use smaller uppercase text with extra
letter spacing.

Bold text has two uses: emphasis in prose and one bold letter that marks a
control's [keyboard shortcut](./interaction-grammar.md#accelerators).

## Panels and controls

Panels have sharp corners, 1 pt dark borders, and 8 pt gaps. They have no
shadows. A panel header is 26 pt tall. Action buttons sit on the right, with
the close button last.

Tables use a strong rule below the header and light rules between rows. A
marked row has a 3 pt bar inside its left padding, so marking it does not move
the text. Marked rows hidden by a filter appear in a separate section above the
visible rows.

A keyboard-focused link has a double underline. A focused button has a grey
background. Containers own spacing; text controls do not add their own padding.

## Motion

Panels and the camera use the same spring animation (`k=800`, `ζ=1`), which
settles in about 330 ms. Changing a target keeps the current speed. New panels
fade and grow into place; closed panels leave a short fading image.

Launcher, workspace, and history overlays use a faster spring (`k=1200`) that
settles in about 200 ms. Their sheet, background, and contents fade together.
Trackpad movement follows the input directly and does not use a spring.

The application requests another frame only while something is moving. The
problems count therefore stays still instead of pulsing indefinitely.
