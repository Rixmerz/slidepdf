# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

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
