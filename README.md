# slidepdf

Ultra-light PPTX to PDF converter in Rust, built for e-learning platforms that
store slides on a CDN.

[![CI](https://github.com/Rixmerz/slidepdf/actions/workflows/ci.yml/badge.svg)](https://github.com/Rixmerz/slidepdf/actions/workflows/ci.yml)



- **Small PDFs.** Text uses the base-14 PDF fonts (zero font bytes), JPEGs pass
  through untouched, oversized images are downsampled to the size they are
  shown at, images are deduplicated, streams use Flate level 9 and the xref is
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
the glyphs used are embedded. Characters that no font can draw become `?` and
are listed in the report.

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

In Node, the WebAssembly build peaks around 140 MB on the 123 MB deck with
`convertFile` (about 50 MB of it is Node itself and 25 MB the compiled module).
For memory-constrained pods, run the native binary as a job; in Node, use
`convertFile` / `convertStream` rather than `convert`, which keeps every
output in memory.

## npm (WebAssembly)

```sh
scripts/build-npm.sh   # needs the wasm32-unknown-unknown target and wasm-bindgen-cli 0.2.130
```

```ts
import { convertFile } from "slidepdf";

// Node, bounded memory: reads from disk, writes slide-NNN.pdf, deck.pdf,
// manifest.json and report.json into the directory as they are produced.
const { manifest, report } = convertFile("course.pptx", "out/course-1", { preset: "balanced" });
```

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
| `slidepdf` | everything | 1.5 MB | 659 KB |
| `slidepdf/lite` | conversion only (no font embedding, no `assemble`/`split`) | 671 KB | 310 KB |

Tested in Node and Chromium. Cloudflare Workers is expected to work with
`slidepdf/lite` (pass the imported `.wasm` module to `ready()`), but has not
been tested yet.

## Viewer

```html
<script type="module" src="slide-viewer.js"></script>   <!-- or: import "slidepdf/viewer" -->
<slide-viewer manifest="/cdn/course-1/manifest.json" show-warnings></slide-viewer>
```

Keys: arrows / PageUp / PageDown / Space, Home, End, `F` for fullscreen.

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

- Convert at upload time, in a job or worker, not on request.
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
deck; 3D charts are drawn flat); previews of embedded
objects (Excel, Word...); and any content wrapped in `mc:AlternateContent`
with a fallback picture (equations, some icons). Hidden shapes are skipped.

Not supported yet (reported per slide): radar, bubble, stock and surface
charts (drawn as line charts), SmartArt saved without its
drawing, EMF/WMF/SVG/TIFF images, built-in Office table styles that the deck
does not define (a default look is used), rotated groups, shadows
and effects, animations and transitions. Text layout is close to PowerPoint
but not identical, since fonts differ unless the real ones are registered.

## Layout

- `src/pptx.rs`: OOXML reader and inheritance resolution into the IR (`src/ir.rs`)
- `src/pdf.rs`: PDF writer, text layout; `src/images.rs`, `src/fonts.rs`
- `src/fragments.rs`: manifest, assemble, split
- `bindings/wasm`, `npm/`: WebAssembly bindings and the npm package
- `tests/`: robustness tests (corrupt, truncated and hostile inputs) and fixture
- `scripts/`: npm build and benchmarks
- `viewer/`: `<slide-viewer>` and the static `index.html`

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
