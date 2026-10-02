# tilcayo

[![CI](https://github.com/pgavlin/tilcayo/actions/workflows/ci.yml/badge.svg)](https://github.com/pgavlin/tilcayo/actions/workflows/ci.yml)

Tilcayo is a low-level terminal GUI runtime built around the
[Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/).
It owns terminal input, graphics output, and session lifecycle while leaving
rendering, widgets, and application policy to its caller.

It provides:

- immutable packed-RGB frames and output-space damage rectangles;
- damage clipping, coalescing, bounding-box reduction, and full-frame promotion;
- a stable Kitty image updated with animation-frame rectangle replacement when supported;
- full-image replacement on terminals limited to baseline Kitty graphics;
- verified POSIX shared-memory and temporary-file transfers for local updates;
- direct transfer with adaptive zlib compression when local media are unavailable;
- capability probing with fragmented-reply parsing and preservation of concurrent startup input;
- optional Kitty logical-DPI discovery for point-sized UI and font scaling;
- raw mode, alternate-screen, keyboard, mouse, focus, paste, and resize setup;
- a single stoppable terminal event reader;
- pixel-pointer mapping through the image placement;
- OSC 52 clipboard output;
- a single-slot latest-frame mailbox and blocking presentation worker; and
- terminal restoration, presentation statistics, and observer hooks.

```rust,no_run
use std::{io, sync::Arc};
use tilcayo::{Event, Frame, Rect, Runtime, RuntimeConfig};

let runtime = Runtime::enter(RuntimeConfig::default())?;
let size = runtime.capabilities().size;
let (width, height) = (
    u32::from(size.width_px.unwrap_or(640)),
    u32::from(size.height_px.unwrap_or(480)),
);
let pixels = Arc::<[u8]>::from(vec![0_u8; width as usize * height as usize * 3]);
let frame = Frame::rgb(
    1,
    width,
    height,
    width as usize * 3,
    pixels,
    vec![Rect::full(width, height)],
)?;
runtime.submit(frame).map_err(|_| io::Error::other("runtime stopped"))?;

if let Event::Paste(text) = runtime.next_event()? {
    let _pasted_text = text;
}
runtime.shutdown()?;
# Ok::<(), io::Error>(())
```

`Runtime` performs capability probing before starting its event reader and owns
normal graphics and control writes thereafter. When Kitty answers its
`dpi_x`/`dpi_y` terminal queries, `runtime.capabilities().logical_dpi` exposes
that logical UI/font DPI; it is not necessarily the monitor's physical DPI.
Lower-level presenter, transport, and event-reader components remain public for
custom integrations. A completed presentation means that the entire command
was written and flushed where required; it is not a display acknowledgement
from the terminal.

## Examples

Run a resize-aware RGB gradient:

```sh
cargo run --release --example gradient
```

Run a damage-tracked mouse painting demo:

```sh
cargo run --release --example paint
```

In either example, press `q` or Escape to exit. In the painting demo, drag with
the mouse to draw, use different mouse buttons for different colors, and press
`c` to clear the canvas.

## License

Tilcayo is licensed under the [MIT License](LICENSE).
