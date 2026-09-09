# Terminal

Open **terminal** from the launcher. It starts the user's login shell in their
home directory, in a real PTY. Each open panel has its own shell. The terminal
requests the full workspace height and starts at half width. **Full width** and
**half width** change the panel's share of the viewport and resize the PTY in
place, keeping the shell, output, cursor, and partially typed command.

Plain keys belong to the terminal. Control chords, arrows, function keys, and
Tab go to the child; the workspace's Command shortcuts still navigate panels.
Option characters and composed text use the normal text input path. Drag to
select, double-click a word, triple-click a line, Command-C to copy, Command-A
to select all, and Command-V to paste. The terminal retains 4,000 scrollback
lines. Paste uses Ghostty's sanitization and bracketed-paste encoding.

When the shell exits, its final output stays visible and **restart shell**
starts another. Closing stops and reaps the process. The saved panel remembers
its width; restoring or undoing a close starts a new shell. Processes and output
are local and ephemeral: they are neither stored nor synchronized.

The terminal app, launcher entry, and native dependencies are excluded from
Android. A terminal restored from another device uses the usual missing-app
card. Scripted runs and panels-library scenes use a deterministic demo shell;
they never launch the user's shell.

## Engine and rendering

The engine is `libghostty-vt` through the pinned `libghostty-vt` 0.2.1 Rust
bindings. Ghostty maintains the cell grid, styles, Unicode graphemes, cursor,
selection, alternate screen, and scrollback. Makepad paints it directly into
the panel's clipped surface. This is the same separation illustrated by
[Ghostling](https://github.com/ghostty-org/ghostling): the VT library has no
renderer or window. Ghostty's full native embedding API is unnecessary here.

Mosaic uses `alacritty_terminal` 0.26.0 with a similar host-drawn grid. It remains
a viable Rust-only alternative. Ghostty was selected to use the preferred
engine now that its VT API supports an independently rendered terminal.
`portable-pty` supplies the shell and PTY; blocking reads, writes, resize calls,
and process cleanup stay on background threads. Output is bounded and wakeups
are coalesced; the UI parses at most 256 KiB per pass.

This is a basic text terminal. Mouse input selects text locally; mouse reporting
to terminal programs, inline images, and the extended Kitty keyboard protocol
are not part of this first UI.

## Building

`mise install` installs Rust and Zig 0.15.2, the version required by the
Ghostty source pinned in the Rust bindings. Use the normal `mise exec -- cargo`
commands. The first build fetches that source and Zig dependencies; the VT
library is statically linked, so users need no Zig installation at runtime.

On Apple Silicon, newer SDKs can expose only `arm64e` in their main libSystem
stub, which Zig 0.15.2 cannot link. The `build-tools/zig` wrapper selects an
installed macOS 14/15 Command Line Tools SDK for Zig alone when needed. The
app and other build tools keep their usual SDK. A machine affected by this
Zig issue needs one of those compatible SDKs installed.
