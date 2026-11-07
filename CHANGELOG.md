# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.4.0] - 2026-11-07

Fidelity and size release: decks made of photos and brand fonts come out
smaller and closer to PowerPoint, and the report says what it could not keep.

### Changed

- A PNG that holds a photo (many distinct colours) is stored as JPEG at the
  preset's quality when that halves it or better; its transparency stays a
  lossless soft mask, with transparent pixels filled from their visible
  neighbours so no fringe shows. Logos, diagrams and screenshots stay
  lossless. An 81 MB deck of photos went from 45 MB to 5.9 MB, another from
  5.8 MB to 515 KB, and conversion got faster (less to compress).
- The weight named in a typeface ("Montserrat Black", "Segoe UI Semibold")
  is used: the run takes the registered font of the nearest weight (same
  slant first), or the bold base-14 font from SemiBold up when none is
  registered. These runs were drawn regular before.
- `FontFile.data` is an `Arc<[u8]>` (Rust API): a conversion no longer
  copies every registered font, and a variable font's instances share it.

### Added

- Variable fonts: a font with a `wght` axis is embedded at the weight each
  run asks for (one subset per weight used). Their default outlines, often
  the Thin master, were drawn before.
- The report lists every typeface drawn with a base-14 font instead (other
  than Arial, Helvetica, Times, Courier and Calibri), once per deck, so a
  deck whose brand font is missing is no longer reported clean.
- Viewer: `requestInit` (fetch options such as an `Authorization` header,
  also passed to pdf.js), `loader(url, kind)` to fetch every file yourself,
  `pdfjsLib` to use a pdf.js 3.x or 4.x module the page already loads, and
  texts in Spanish (`lang="es"` on the element or an ancestor) or your own
  (`labels`). Types for the element in `index.d.ts`.

### Fixed

- `npm test` runs on Node 22 and 24 again (the directory argument broke it).

## [0.3.0] - 2026-11-07

Scalability release: everything a converter needs to run predictably inside
memory-limited pods. No change to the PDFs produced.

### Added

- A memory budget: `slidepdf::budget` is a counting allocator (installed by
  the CLI and the WebAssembly bindings; library users opt in) with a hard
  cap. `--memory-limit <mb>` in the CLI aborts with exit code 134 and a
  message on stderr; in npm, `setMemoryLimit(mb)` and the pool's `memoryMb`
  fail the conversion with code `memory` and leave the process up (Node
  reloads the instance). `memoryStats()` and `stats.peakBytes` report the
  peak; the CLI prints it in its summary.
- Timeouts and cancellation: `--timeout <seconds>` in the CLI (exit code
  124); `convert_stream_with` takes a progress hook that can stop the
  conversion after any slide; in npm, `timeoutMs`, `signal` (an
  `AbortSignal`) and `onProgress` on `convertStream`, `convertFile` and the
  pool, with codes `timeout`, `aborted` and `cancelled`.
- A worker pool for Node: `createPool({ size, memoryMb, timeoutMs,
  recycleAboveMb })` runs conversions in worker threads, queues the rest,
  replaces workers whose WebAssembly memory grew past the cap, and compiles
  the module once, eagerly, so a fresh worker is as fast as a warm one.
  `convertFileInWorker` for one-off jobs. `pool.stats()` for metrics.
- `inspect` / `inspectFile` / `slidepdf inspect`: slide count, page size,
  part and media bytes from the zip directory alone, in milliseconds, to
  route or refuse a deck before converting it.
- `SlidepdfError` with a stable `code` on every failure in npm; `stats`
  (`ms`, `peakBytes`, `slides`, `inputBytes`) on every conversion result.
- `--quiet` in the CLI.
- A `Dockerfile` that builds a scratch image (about 3 MB) with the CLI.
- The demo server converts through the pool, refuses decks over a slide
  limit after `inspectFile`, cancels when the client disconnects and maps
  error codes to HTTP statuses.

### Changed

- `convertStreamRaw` (the raw WebAssembly export) takes an optional
  `onProgress` argument; the typed API is unchanged apart from the additions.

## [0.2.0] - 2026-11-07

### Added

- Charts are drawn from the data cached in the deck: clustered, stacked and
  100% stacked columns and bars, lines (with markers), areas, pies, doughnuts
  and scatter plots, with titles, legends, axes with rounded scales,
  gridlines, data labels, per-point and per-series colors, gap and overlap
  like Office, and the chart's own background (color, picture or texture).
  3D charts are drawn flat and reported. In the 539-deck test corpus no chart
  is skipped any more (25 before).
- EMF and WMF pictures (logos, clip art, previews of embedded Excel/Word
  objects) are replayed into vector shapes, text and embedded bitmaps:
  pens, brushes, fonts, window/viewport mapping, world transforms, paths,
  polygons, Béziers, rectangles, ellipses, pattern fills and DIBs. Clipping
  regions and non-copy raster operations are not supported. In the corpus no
  metafile is skipped any more (25 before).

## [0.1.0] - 2026-11-06

### Added

- PPTX to PDF conversion with slide, layout, master and theme inheritance:
  text styles, bullets and numbering, autofit, theme colors, preset and custom
  shapes, rotation and flips, groups, pictures (JPEG, PNG with alpha,
  cropping), tables, backgrounds and slide numbers.
- Size-oriented PDF output: base-14 fonts, JPEG passthrough, downsampling to
  the displayed resolution, image deduplication, compact objects and a
  compressed cross-reference stream.
- Presets `light`, `balanced` and `print`.
- Optional font embedding (subset CIDFontType2 with ToUnicode).
- Per-slide fragments with a manifest (order, size, hash, title, text),
  `assemble` (with resource deduplication) and `split`.
- Fidelity report per slide.
- Streaming conversion (`convert_stream`) with bounded memory, plus limits
  against hostile input (zip bombs, oversized images, deep XML nesting).
- `slidepdf` command line tool.
- npm package with WebAssembly builds for Node, browsers and Workers, in a
  full and a `lite` variant, with TypeScript types.
- Fidelity from a 539-deck corpus: SmartArt via its saved drawing, previews
  of embedded objects, GIF images, gradients (linear and radial), fill
  opacity, theme fill styles, deck-defined table styles, vertical and rotated
  text, hue/saturation color modifiers, about 30 more preset shapes, hidden
  shapes skipped, Calibri metrics for line breaking, hyphen line breaks.
- Streaming `assemble` and per-page `split` (bounded memory); password
  protected PDFs are reported instead of producing empty output.
- `<slide-viewer>` web component: lazy loading, navigation, large-slide and
  fullscreen modes, thumbnails, search, tracking events, and styling through
  CSS variables, `::part()` and slots.
