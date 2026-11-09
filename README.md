# slidepdf

Ultra-light PPTX to PDF converter in Rust, built for e-learning platforms that
store slides on a CDN.

[![CI](https://github.com/Rixmerz/slidepdf/actions/workflows/ci.yml/badge.svg)](https://github.com/Rixmerz/slidepdf/actions/workflows/ci.yml)



- **Small PDFs.** Text uses the base-14 PDF fonts (zero font bytes), JPEGs pass
  through untouched, photos saved as PNG become JPEG, oversized images are
  downsampled to the size they are shown at, images are deduplicated, streams use Flate level 9 and the xref is
  a stream. A 4-slide test deck with two pictures is 16 KB (10 KB with the
  `light` preset); text-only slides are under 1 KB. A 7.9 MB deck with a
  4000x3000 photo becomes 42 KB (`light`), 97 KB (`balanced`) or 1 MB (`print`).
- **Fragments.** Every slide is also a standalone one-page PDF plus a
  `manifest.json` (order, size, content hash, title, full text). Store them on
  the CDN, show them one by one, and rebuild any range into one PDF with
  `assemble`, which deduplicates shared images and fonts again (4 fragments of
  28 KB rebuild into a 15.7 KB PDF). `split` turns any existing PDF into
  fragments.
- **Viewer.** `<slide-viewer>` web component on top of pdf.js: lazy loading,
  previous/next buttons, keyboard and swipe, large-slide and fullscreen modes,
  thumbnails rendered in the browser, search, fidelity warnings and tracking
  events (`slidechange`, `slideviewed`, `completed`). Restyle it with CSS
  variables, `::part()` and slots.
- **Fidelity report.** What could not be reproduced (charts without fallback
  image, unsupported image formats, characters outside the built-in fonts...),
  per slide, so uploaders know before publishing.
- **Bounded memory.** The PDF is streamed page by page, images are read
  from the package only when drawn, large JPEGs are decoded at reduced scale
  and PNGs are downsampled row by row. Peak memory depends on the largest
  image, not on the size of the deck: a 123 MB, 60-slide deck converts in
  26-47 MB. Hostile input (zip bombs, huge images, deep XML) is refused.
- **Everywhere.** Rust library and CLI; an npm package (WebAssembly) for Node,
  browsers and Workers with TypeScript types.

## CLI

```sh
cargo build --release
slidepdf convert   deck.pptx -o deck.pdf --preset light
slidepdf fragments deck.pptx -o out/          # slide-NNN.pdf, manifest.json, report.json, deck.pdf, index.html
slidepdf fragments existing.pdf -o out/       # split any PDF into fragments
slidepdf assemble  out/ --range 2-5,8 -o part.pdf   # verifies hashes against the manifest
slidepdf report    deck.pptx [--json]
slidepdf search    out/ equipo proteccion     # accent- and case-insensitive
```

Options: `--preset light|balanced|print`, `--font Brand.ttf` (repeatable),
`--hidden`, `--no-viewer`, `--no-pdf`, `--cache-mb N`.

| Preset | Images | JPEG quality | Fonts |
|---|---|---|---|
| `light` | 110 DPI | 65 | base-14 only |
| `balanced` (default) | 150 DPI | 75 | base-14 + registered fonts |
| `print` | 300 DPI | 88 | base-14 + registered fonts |

### Fonts

Without registered fonts, text is drawn in Helvetica / Times / Courier
(matched by the deck's typeface), which covers Spanish and Western European
text. Register a TTF/OTF with `--font` to use the real typeface (matched by
family name) and to cover characters outside WinAnsi (Greek, arrows...). Only
the glyphs used are embedded. A character outside WinAnsi is drawn with any
registered font that has the glyph when the run's own typeface is not
registered or lacks it (same slant first, then the nearest weight, then
registration order), so registering one broad font such as Noto Sans gives
Greek and Cyrillic to every typeface, while Latin text keeps using the base-14
fonts. Characters that no font can draw become `?` and are listed in the
report.

Weights written in the typeface ("Montserrat Black", "Segoe UI Semibold")
count: the run takes the registered file of the nearest weight, or the bold
base-14 font from SemiBold up when none is registered. A variable font
(`Montserrat[wght].ttf`) is embedded at the weight each run asks for. Every
typeface drawn with a base-14 font instead (other than Arial, Helvetica,
Times, Courier and Calibri) is listed in the report, since its line breaks
may differ from PowerPoint's.

### Images

JPEGs are kept as they are unless much larger than the size they are shown
at. A PNG that holds a photo (many distinct colours) is stored as JPEG at the
preset's quality when that halves it or better; its transparency stays a
lossless mask. Logos, diagrams and screenshots stay lossless.

## Performance and memory

Measured with `scripts/bench.sh` (release CLI, one core, Linux x86-64):

| Deck | Preset | Time | Peak memory | deck.pdf |
|---|---|---|---|---|
| 60 slides, 30 photos 3000x2000, 123 MB | light | 2.2 s | 26 MB | 2.0 MB |
| same | balanced | 2.7 s | 32 MB | 6.3 MB |
| same | print | 1.1 s | 47 MB | 123 MB |
| 1 slide, 8000x8000 PNG with alpha | light | 0.11 s | 12 MB | 29 KB |
| 2 slides, 4000x3000 JPEG + 3000x3000 PNG | balanced | 0.2 s | 16 MB | 94 KB |

`assemble` streams too: rebuilding the 123 MB `print` deck from its 60
fragments peaks at 26 MB. `split` of an existing PDF keeps the input document
in memory (about 2.5x its size); fragments split from a PDF each carry their
own copy of embedded fonts, which `assemble` deduplicates again.

`print` keeps photos that are already close to 300 DPI untouched (no
re-encoding), which is why it is faster and the PDF is as large as the photos.

What bounds memory:

- the input is read through `Read + Seek` (`convert_stream`, `slidepdf` CLI,
  `convertFile` in npm), never loaded whole;
- each image is fetched when drawn, prepared and released; prepared images
  are reused across slides within `cache_bytes` (`--cache-mb`, default 16);
- outputs are written as they are produced.

Limits on hostile input: 64 MB per package part, 1 GB inflated in total,
20,000 entries, 5,000 slides, 80 megapixels per image, XML nesting of 256.
Progressive JPEGs cannot be decoded row by row (about 3.5 MB per megapixel),
so those above 20 megapixels are skipped with a report warning asking for a
baseline JPEG. GIFs are capped at 16 megapixels (interlaced GIFs decode whole;
non-interlaced ones row by row).

Tested against 539 real-world decks (the LibreOffice and Apache POI test
suites): no crashes or hangs; the 11 rejected files are corrupt archives.
About 2,900 mutated variants of feature-rich decks (byte flips inside XML and
media, hostile numbers, truncations) in release and debug builds: no crashes.

In Node, use `convertFile` / `convertStream` rather than `convert`, which
keeps every output in memory; and for anything that serves more than one
deck, the worker pool below.

## Running in pods

Four things make a converter predictable inside a memory-limited container,
and all four are built in:

1. **A hard memory cap per conversion.** The library counts every heap
   allocation (`slidepdf::budget`). Past the cap, the conversion fails on
   its own instead of growing until the orchestrator kills the pod:
   `--memory-limit <mb>` in the CLI (exit code 134 and a message on stderr),
   `memoryMb` in the Node pool, `setMemoryLimit(mb)` on the direct API.
2. **A timeout.** `--timeout <seconds>` in the CLI (exit code 124);
   `timeoutMs` in Node, which is cooperative: the converter checks the clock
   after every slide and stops with code `timeout`, so the worker survives
   and the overrun is at most one slide. The pool kills a worker that does
   not stop within `killGraceMs` (default 5 s). `signal` (an `AbortSignal`)
   cancels the same way.
3. **Isolation.** In Node, `createPool({ size })` runs conversions in
   worker threads, at most `size` at a time (the rest queue). WebAssembly
   memory never shrinks, so a worker that grew past `recycleAboveMb`
   (default 64) is replaced; a replacement costs about 15 ms. Process memory
   is then bounded by roughly `size x (memoryMb + 30 MB) + 70 MB`.
4. **A preflight.** `inspect` / `inspectFile` / `slidepdf inspect` read only
   the zip directory and `presentation.xml` (12 MB and a few milliseconds
   on the 123 MB deck) and return slide count, page size and media bytes, so
   a server can refuse or re-route a deck before taking a conversion slot.

```ts
import { createPool } from "slidepdf";

const pool = createPool({ size: 2, memoryMb: 256, timeoutMs: 120_000 });

const info = await pool.inspectFile(upload);       // { slides, mediaBytes, largestMediaBytes, ... }
if (info.slides > 500) throw new Error("too many slides");

try {
  const { manifest, stats } = await pool.convertFile(upload, outDir, {
    preset: "balanced",
    signal: request.signal,
    onProgress: (done, total) => console.log(`${done}/${total}`),
  });
  console.log(stats);                                // { ms, peakBytes, slides, inputBytes }
} catch (e) {
  switch (e.code) {                                  // SlidepdfError
    case "memory":  /* 413: the deck needs more than memoryMb */ break;
    case "timeout": /* 408 */ break;
    case "aborted": /* client went away */ break;
    case "convert": /* 422: not a PPTX, corrupt, over the hard limits */ break;
  }
}
```

Timing stays flat across workers: the pool compiles the module once, eagerly
(`eagerCompile`, a process-wide V8 flag that costs about 13 MB), and hands
the same compiled module to every worker, so a fresh worker runs as fast as
a warm one. Create the pool before calling the direct API (`convertFile`,
`inspectFile`) in the same process, otherwise V8 reuses the lazily compiled
module and the first conversion in each worker is slower. Measured on the 123 MB deck with `balanced`, three fresh workers
in a row: 4.9 s, 4.8 s, 4.7 s (6.7 s cold and 4.7 s warm without it), peak
heap 19 MB each, process RSS back to 144 MB after each worker is retired.

The native CLI is the lightest option for a job runner: 27 MB RSS on the
123 MB deck, and `Dockerfile` builds a scratch image of 4.3 MB (the image
runs as an unprivileged user; pass `--user` to write into a directory you own):

```sh
docker build -t slidepdf .
docker run --rm -m 64m --user "$(id -u):$(id -g)" -v "$PWD:/work" slidepdf \
  fragments /work/deck.pptx -o /work/out --memory-limit 32 --timeout 60 --quiet
```

Every failure has a stable code (`SlidepdfError.code` in npm, the exit code in
the CLI): `convert`, `memory`, `timeout`, `aborted`, `cancelled`, `trap`,
`worker`, `closed`.

## npm (WebAssembly)

```sh
scripts/build-npm.sh   # needs the wasm32-unknown-unknown target and wasm-bindgen-cli 0.2.130
```

```ts
import { convertFile } from "slidepdf";

// Node, bounded memory: reads from disk, writes slide-NNN.pdf, deck.pdf,
// manifest.json and report.json into the directory as they are produced.
const { manifest, report, stats } = convertFile("course.pptx", "out/course-1", { preset: "balanced" });
```

Servers should use `createPool` instead (worker threads with a memory cap,
timeout and cancellation per job): see "Running in pods" above.

```ts
import { convert, convertStream, assemble, search, ready } from "slidepdf";

await ready();                         // no-op in Node; loads the .wasm in browsers/Workers
const out = convert(pptxBytes, { preset: "balanced", fonts: [brandTtf] });
out.pdf;                               // Uint8Array, full PDF
out.fragments;                         // [{ number, file, pdf, hash, title, text, warnings }]
out.manifestJson;                      // store next to the fragments
out.report.text;                       // human-readable fidelity summary
const part = assemble([f2, f3]);       // any PDFs, in order
search(out.manifest, "equipo");        // [{ number, title, file }]
```

`convertStream(source, options, { onFragment, onPdf })` is the bounded
version for any runtime: `source` is bytes or `{ size, read(offset, length) }`.
Likewise `assembleStream(next, onPdf)` merges with bounded memory, and in Node
`assembleFiles(paths, outPath)` rebuilds the 123 MB `print` deck in a
160 MB process (781 MB with the in-memory `assemble`). `split` in npm keeps
the input PDF and its pages in memory.

Two builds ship in the package:

| Import | Includes | .wasm | gzipped |
|---|---|---|---|
| `slidepdf` | everything | 1.6 MB | 686 KB |
| `slidepdf/lite` | conversion only (no font embedding, no `assemble`/`split`) | 735 KB | 335 KB |

Tested in Node and Chromium. Cloudflare Workers is expected to work with
`slidepdf/lite` (pass the imported `.wasm` module to `ready()`), but has not
been tested yet.

## Viewer

```html
<script type="module" src="slide-viewer.js"></script>   <!-- or: import "slidepdf/viewer" -->
<slide-viewer manifest="/cdn/course-1/manifest.json" show-warnings></slide-viewer>
```

Keys: arrows / PageUp / PageDown / Space, Home, End, `F` for fullscreen.

Files behind authentication, your own pdf.js, your own texts (set these
properties from JavaScript, before or after `manifest`):

```js
import * as pdfjsLib from "pdfjs-dist";            // 3.x or 4.x, worker set up by you
const v = document.querySelector("slide-viewer");
v.pdfjsLib = pdfjsLib;                              // default: pdf.js 3 from cdnjs (`pdfjs` attribute)
v.requestInit = { headers: { Authorization: `Bearer ${token}` } };
// or fetch every file yourself (kind is "manifest" or "slide"):
v.loader = (url, kind) => fetch(api.mediaUrl(url), { headers });
v.labels = { search: "Find a slide" };              // override any built-in text
```

The built-in texts are English, or Spanish with `lang="es"` on the element
or an ancestor.

Events (`event.detail`): `ready {count}`, `slidechange {number, previous}`,
`slideviewed {number, ms}` (when leaving a slide), `completed {total, ms}`
(every slide stayed on screen at least `view-threshold` ms, default 2000).

Styling, from simplest to most control:

```css
slide-viewer { --sv-bg: #fff; --sv-fg: #222; --sv-accent: #0a7; --sv-radius: 12px; }
slide-viewer::part(button) { border-radius: 999px; }
slide-viewer::part(thumb-current) { border-color: #0a7; }
```

- Variables: `--sv-bg --sv-fg --sv-muted --sv-accent --sv-panel --sv-border
  --sv-input-bg --sv-radius --sv-button-radius --sv-button-bg --sv-button-fg
  --sv-nav-bg --sv-nav-size --sv-thumb-width --sv-font --sv-page-shadow`
- Parts: `frame stage page nav nav-prev nav-next bar button button-prev
  button-next button-focus button-fullscreen counter title search warning
  thumbs thumb thumb-current thumb-seen thumb-number`
- Slots: `prev-icon next-icon focus-icon fullscreen-icon bar-start bar-end`
- Attribute `unstyled` drops the built-in look.

See `examples/angular-demo` for an Angular 22 + Node integration.

## Production notes

- Convert at upload time, in a job or worker, not on request; in Node use
  `createPool` with `memoryMb` and `timeoutMs` set, and `inspectFile` to
  refuse oversized decks first.
- Store `slide-NNN.pdf`, `deck.pdf` and `manifest.json` on the CDN with
  long-lived caching; fragments are immutable and the manifest carries their
  hashes.
- Show `report` to the person who uploads the deck before publishing it.
- Keep upload size limits at the HTTP layer too; the converter's own limits
  are a last line of defense.

## What is supported

Text with inheritance from layout, master and theme (sizes, colors, bold,
italic, underline, alignment, bullets and numbering, line spacing, autofit,
vertical and rotated text); Calibri text is laid out with Calibri's real
metrics (from the metric-compatible Carlito) so lines break where PowerPoint
breaks them; theme colors with luminance, hue and saturation modifiers;
solid, translucent, linear and radial gradient, and picture fills, including
theme fill styles; about 60 preset shapes and custom geometry; rotation and
flips; groups; pictures (JPEG, PNG and GIF with transparency, cropping);
tables with the styles defined in the deck; backgrounds; slide numbers;
SmartArt (from the drawing PowerPoint saves with it); charts (columns, bars,
lines, areas, pies, doughnuts and scatter, redrawn from the data cached in the
deck; 3D charts are drawn flat); EMF and WMF pictures as vector shapes, text and
embedded bitmaps (no clipping regions; text in non-Latin code pages may be
garbled); previews of embedded
objects (Excel, Word...); and any content wrapped in `mc:AlternateContent`
with a fallback picture (equations, some icons). Hidden shapes are skipped.

Not supported yet (reported per slide): radar, bubble, stock and surface
charts (drawn as line charts), SmartArt saved without its
drawing, SVG and TIFF images, built-in Office table styles that the deck
does not define (a default look is used), shadows
and effects, animations and transitions. Text layout is close to PowerPoint
but not identical, since fonts differ unless the real ones are registered.

## Layout

- `src/pptx.rs`: OOXML reader and inheritance resolution into the IR (`src/ir.rs`)
- `src/pdf.rs`: PDF writer, text layout; `src/images.rs`, `src/fonts.rs`
- `src/fragments.rs`: manifest, assemble, split
- `bindings/wasm`, `npm/`: WebAssembly bindings and the npm package
- `tests/`: robustness tests (corrupt, truncated and hostile inputs) and fixture
- `scripts/`: npm build, benchmarks and the corpus fetcher
- `viewer/`: `<slide-viewer>` and the static `index.html`

## Real-deck corpus test

CI converts about 540 real decks (the LibreOffice `sd/qa/unit/data` and Apache
POI `test-data/slideshow` test files) on every pull request and compares each
result with `tests/corpus-baseline.json`. To run it locally, fetch the corpus
once (about 180 MB) and point the ignored test at it:

```sh
scripts/corpus.sh /tmp/corpus
SLIDEPDF_CORPUS=/tmp/corpus cargo test --release --test corpus -- --ignored
```

A deck that panics, exceeds the time cap or the 256 MB memory budget, or whose
slide count disagrees with `inspect`, fails the test. Any other difference
from the baseline fails it too and is printed as decks that got better or
worse. If the change is intended, refresh the baseline with
`SLIDEPDF_CORPUS_UPDATE=1` on the same command and commit the file.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
