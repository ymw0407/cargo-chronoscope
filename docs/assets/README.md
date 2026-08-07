# Demo assets

This directory stores README media and the source workflow needed to recreate
it. Keep generated GIFs small enough for GitHub to render inline.

## `watch-demo.gif`

The README hero GIF shows a short `cargo-chronoscope` session against a Rust
workspace:

1. `cargo-chronoscope ls`
2. `cargo-chronoscope watch`
3. `cargo-chronoscope ls`
4. `cargo-chronoscope diff <before> <after>`

The current GIF was captured manually because VHS does not yet record the
ratatui alt-screen `watch` dashboard reliably. Non-TUI commands record in VHS,
but the live dashboard renders blank under its terminal bridge.

## Re-recording workflow

Use a workspace large enough to keep the `watch` dashboard visible for a few
seconds. A clone of `ripgrep` works well.

```bash
git clone https://github.com/BurntSushi/ripgrep.git /tmp/ripgrep-demo
cd /tmp/ripgrep-demo
cargo clean
```

Record the terminal at 1100 px wide with a readable font size. On Windows, the
existing GIF used Windows Terminal plus Game Bar. Save the source capture as:

```text
docs/assets/watch-demo.mp4
```

Suggested command sequence for the capture:

```bash
cargo-chronoscope ls
cargo clean
cargo-chronoscope watch
cargo-chronoscope ls
cargo-chronoscope diff <before-build-id> <after-build-id>
```

For a stronger demo, make sure at least one `slower` or `faster` anomaly marker
appears in the `watch` dashboard. If natural variance is too low, record a few
baseline builds first, then perturb one crate or run under background load for
the captured build.

Convert the source MP4 to a GIF with a generated palette:

```bash
ffmpeg -y -i docs/assets/watch-demo.mp4 \
  -vf "fps=10,scale=1100:-1:flags=lanczos,palettegen=max_colors=128" \
  docs/assets/watch-demo-palette.png

ffmpeg -y -i docs/assets/watch-demo.mp4 -i docs/assets/watch-demo-palette.png \
  -lavfi "fps=10,scale=1100:-1:flags=lanczos[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=3" \
  docs/assets/watch-demo.gif
```

Check the result before committing:

```bash
file docs/assets/watch-demo.gif
du -h docs/assets/watch-demo.gif
```

The committed GIF should stay at or below 1.5 MB. Do not commit the temporary
palette PNG. Commit the source MP4 only when the GIF cannot be reproduced from
the documented steps alone or when a reviewer asks for it.
