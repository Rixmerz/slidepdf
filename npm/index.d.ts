export type Bytes = Uint8Array | ArrayBuffer | ArrayBufferView;

export type Preset = "light" | "balanced" | "print";

export interface ConvertOptions {
  /** light: 110 DPI images, base-14 fonts. balanced (default): 150 DPI. print: 300 DPI. */
  preset?: Preset;
  /** Override the preset's image resolution. 0 keeps original pixels. */
  imageDpi?: number;
  /** Override the preset's JPEG quality (1-100) for re-encoded images. */
  jpegQuality?: number;
  /** TTF/OTF files to embed as subsets (matched by family name, also used as fallback). Ignored by "light". */
  fonts?: Bytes[];
  includeHidden?: boolean;
  /** Build the full PDF (default true). */
  fullPdf?: boolean;
  /** Build one PDF per slide (default true). */
  fragments?: boolean;
  /** Name stored in the manifest. */
  sourceName?: string;
  /** Memory (MB) for reusing prepared images across slides. Default 16. */
  cacheMb?: number;
}

/** Random-access input, so large decks never have to be loaded whole. */
export interface ByteSource {
  size: number;
  read(offset: number, length: number): Uint8Array;
}

export interface StreamCallbacks {
  /** Receives each slide PDF as soon as it is ready. */
  onFragment: (fragment: Fragment) => void;
  /** Receives the full PDF in pieces. Omit to skip the full PDF. */
  onPdf?: (chunk: Uint8Array) => void;
  /**
   * Pass bytes as views into WebAssembly memory instead of copies. Saves
   * memory, but each view is only valid until the callback returns: write
   * it out synchronously or copy it. Default false.
   */
  zeroCopy?: boolean;
}

export interface StreamResult {
  manifest: Manifest;
  manifestJson: string;
  report: Report;
}

export interface Fragment {
  /** 1-based slide number. */
  number: number;
  /** Suggested file name, e.g. "slide-001.pdf". */
  file: string;
  /** A standalone one-page PDF. */
  pdf: Uint8Array;
  /** Content hash recorded in the manifest (FNV-1a, for integrity, not security). */
  hash: string;
  title: string | null;
  text: string;
  warnings: string[];
}

export interface ManifestSlide {
  number: number;
  file: string;
  bytes: number;
  hash: string;
  title: string | null;
  text: string;
  warnings: string[];
}

export interface Manifest {
  version: 1;
  source: string;
  /** Page size in PDF points. */
  width: number;
  height: number;
  pdf?: { file: string; bytes: number };
  count: number;
  warnings: string[];
  slides: ManifestSlide[];
}

export interface Report {
  slides: number;
  /** true when nothing was simplified or skipped. */
  clean: boolean;
  general: string[];
  perSlide: { slide: number; warnings: string[] }[];
  /** Human-readable summary. */
  text: string;
}

export interface Conversion {
  /** Full PDF (empty when fullPdf is false). */
  pdf: Uint8Array;
  fragments: Fragment[];
  manifest: Manifest;
  /** The manifest exactly as it should be stored (manifest.json). */
  manifestJson: string;
  report: Report;
}

/** Loads the WebAssembly module. Required once in browsers/Workers; a no-op in Node. */
export function ready(module?: WebAssembly.Module | URL | string | BufferSource): Promise<void>;

/** Converts a PPTX into a full PDF, per-slide fragments, a manifest and a fidelity report. */
export function convert(pptx: Bytes, options?: ConvertOptions): Conversion;

/**
 * Bounded-memory conversion: reads the deck through `source` and hands out
 * outputs as they are produced. Use this on servers with memory limits.
 */
export function convertStream(source: ByteSource | Bytes, options: ConvertOptions, callbacks: StreamCallbacks): StreamResult;

/** Node only: converts a .pptx file into `outDir` (slide-NNN.pdf, deck.pdf, manifest.json, report.json) with bounded memory. */
export function convertFile(input: string, outDir: string, options?: ConvertOptions): StreamResult;

/**
 * Bounded-memory merge: `next()` returns the next PDF or null when done;
 * the output arrives in pieces through `onPdf`. Returns the page count.
 */
export function assembleStream(next: () => Bytes | null, onPdf: (chunk: Uint8Array) => void, options?: { zeroCopy?: boolean }): number;

/** Node only: merges PDF files into `outPath` with bounded memory. Returns the page count. */
export function assembleFiles(inputs: string[], outPath: string): number;

/** Merges PDFs (stored fragments or any PDF) in order, deduplicating images and fonts. Keeps everything in memory; prefer assembleStream/assembleFiles for large inputs. */
export function assemble(parts: Bytes[]): Uint8Array;

/** Splits any PDF into one standalone PDF per page. */
export function split(pdf: Bytes): Uint8Array[];

export function fragmentHash(pdf: Bytes): string;

/** Checks a stored fragment against its manifest hash. */
export function verifyFragment(pdf: Bytes, hash: string): boolean;

/** Accent- and case-insensitive search over a manifest's text index. */
export function search(manifest: Manifest, query: string): { number: number; title: string | null; file: string }[];

// <slide-viewer> custom element (import "slidepdf/viewer").
export interface SlideViewerEventMap {
  ready: CustomEvent<{ count: number }>;
  slidechange: CustomEvent<{ number: number; previous: number | null }>;
  slideviewed: CustomEvent<{ number: number; ms: number }>;
  completed: CustomEvent<{ total: number; ms: number }>;
}
