# Morsh terminal reuse assessment

Source reviewed: `/home/captain/projects/morsh`, October 17, 2026. Read-only code inspection; no source was copied, changed, compiled, or tested in this review.

## Recommended reuse

Use morsh's PTY implementation as the starting reference for Agentworks' terminal runtime. Keep the module independent of morsh's UDP transport, protobuf state synchronization, local prediction, and native terminal client. The browser continues to use xterm.js; the host service owns the persistent PTY and a live Ghostty emulator. Reconnection attaches a fresh browser terminal using a redraw of the current screen, with no MVP requirement to restore historical scrollback.

| Source | Useful behavior | Agentworks adaptation |
| --- | --- | --- |
| `crates/morsh-server/src/pty.rs` | `portable-pty` spawn, direct argument arrays, dedicated reader thread, bounded output channel, ConPTY child-exit watcher | Add explicit project working directory, provider-scoped environment, setup-failure cleanup, cancellable worker ownership, and authoritative child exit status |
| `crates/morsh-server/src/server.rs` | Batched input writes, coalescing adjacent resizes while preserving input order, in-place terminal resize | Accept browser input chunks without splitting escape sequences; perform potentially blocking writes outside the async API loop; apply application size/resource limits |
| `crates/morsh-terminal/src/snapshot.rs` | Screen cells, colors, cursor, title, bracketed paste and mouse/focus modes | Use as the visible-screen attachment snapshot while keeping the host emulator alive; no full emulator serialization required |
| `crates/morsh-terminal/src/display.rs` | Snapshot-to-VT redraw/diff and regression tests for cursor position and wide glyphs | Reuse relevant fixtures/test cases and test against xterm.js, rather than assuming compatibility between emulators |
| `crates/morsh-server/src/daemon/{unix,windows}.rs` | Examples of platform-specific detachment and Windows handle behavior | Reference for later host lifecycle work; use a Linux user service for persistent hosting first |

## Required changes and checks

- Morsh's PTY reader reports EOF/read errors as `Exited(0)`. For agent sessions, distinguish output closure, read failure, and actual child exit; drain final output and record the real process status. Windows may deliver reader and process-exit events separately, so terminal lifecycle must tolerate either order.
- Morsh's spawn function starts the child before acquiring all reader/writer handles. Define cleanup on partial initialization failure and join/close owned workers on shutdown.
- Morsh's server performs synchronous PTY writes in its async handler. Agentworks should isolate blocking I/O and bound its input queue so one terminal cannot stall all sessions.
- Morsh's current shutdown calls `child.kill()`. Agentworks additionally needs graceful interrupt, bounded termination, child reaping, and tested process-tree cleanup.
- Resize clamping in morsh allows dimensions up to `u16::MAX`. Select practical Agentworks limits before allocating screen state, and surface resize errors.
- Keep Ghostty alive with the PTY while browsers disconnect. The screen snapshot is a presentation redraw for a fresh browser, not an emulator checkpoint; missing scrollback serialization is outside MVP scope. Continue using the live server emulator for scrolling-region interpretation and display updates.
- Feed PTY output into Ghostty and send generated display updates to xterm.js. Morsh currently lacks host-query replies in its Ghostty path; verify and implement the responses required by both agent CLIs, with one authoritative responder. Do not silently switch to raw PTY forwarding after a presentation-only redraw, which may omit emulator internals such as the scrolling region.
- Preserve output sequence boundaries and input ownership during reconnect; a redraw alone does not provide durable history or prevent duplicate input.

## Integration steps

1. Adapt the focused PTY lifecycle patterns into an Agentworks-owned module during Phase 3; preserve source attribution and applicable license notices if code is copied. Morsh's workspace declares `GPL-3.0-or-later`.
2. Add the working-directory/environment controls needed by provider sessions, then verify both actual CLIs with resize, Unicode, bracketed paste, modified keys, child exit, and final-output drain.
3. Test fresh browser attachment: full redraw of current screen, correct cursor and input modes, ordered later display updates, resize, and continued operation while disconnected. Keep historical scrollback restoration outside MVP.
4. Carry forward the Windows-specific findings behind platform boundaries, with actual Windows validation deferred until that platform is added.

No need to port morsh's network protocol: Agentworks already uses browser WebSockets, and remote hosts use an SSH-launched worker linked over WebSocket.
