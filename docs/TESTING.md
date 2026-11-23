# Testing

```
cargo test --bin monogram-desktop            # unit tests; no display needed
cargo build --bins                           # the sandboxed helpers are separate binaries
tools/headless-gtk-test.sh --bin monogram-desktop -- --ignored <test name>
```

Many tests are `#[ignore]`d because they need a display, GStreamer decoders, bubblewrap or the
built helpers. Run those through `tools/headless-gtk-test.sh`, one test (or one module) per
process, because GTK can only be owned by one thread per process.

## Headless display

`tools/headless-gtk-test.sh` starts KWin's `--virtual` compositor, a real Wayland compositor with
no output, inside a private D-Bus session. No window appears on a screen, and the tests do not
touch your session bus (so no portals, keyring or thumbnailers are started). The compositor
provides the frame clock and mapping that timeline rendering and animation depend on.
Everything the session prints goes to a log that is shown when it ends, so a leftover process the
private bus started cannot hold the caller's pipe open.

- `HEADLESS_BACKEND=broadway` uses GTK's in-memory Broadway display instead. It is fine for
  widget-only tests, but with no viewer it does not tick the frame clock, so tests that wait for
  timeline rows to render stall.
- `HEADLESS_BACKEND=weston` uses Weston's headless backend (pixman renderer, no GPU or X server).
  It is picked automatically when `kwin_wayland` is missing, and it is what `default.nix` starts
  for the Nix build checks, so NixOS needs no X server at all.
- The `#[ignore]`d tests that inject input with `xdotool` (focus, clicks, key presses) are the only
  remaining X11 users; they need an isolated X server (for example `xvfb-run`) until they are ported
  to a Wayland input path. Nothing in `default.nix` or the release workflow runs them.
- The script only ever stops the compositor it started.
