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
- Tests that need `xdotool`, a keyring, or a 60 Hz X11 clock still need a real isolated X server;
  `default.nix` shows how CI starts one with Xvfb.
- The script only ever stops the compositor it started.
