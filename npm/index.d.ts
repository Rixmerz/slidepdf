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
  /**
   * Stop with code "timeout" once the conversion has run this long. Checked
   * after every slide, so the overrun is at most one slide.
   */
  timeoutMs?: number;
  /** Stop with code "aborted" after the current slide once this fires. */
  signal?: AbortSignal;
}

export interface ConversionStats {
  /** Wall time of the conversion. */
  ms: number;
  /** Highest heap use during the conversion. */
  peakBytes: number;
  slides: number;
  inputBytes: number;
}

/** What `inspect` learns from the zip directory and presentation.xml alone. */
export interface Inspection {
  /** Slides listed by the presentation, hidden ones included. */
  slides: number;
  /** Page size in PDF points. */
  width: number;
  height: number;
  parts: number;
  /** Declared uncompressed size of every part. */
  bytes: number;
  media: number;
  mediaBytes: number;
  /** The biggest media part: the one that drives peak memory. */
  largestMediaBytes: number;
  largestMedia: string;
  embeddedFonts: number;
}

export type SlidepdfErrorCode =
  /** The deck could not be converted (not a PPTX, corrupt, over the hard limits). */
  | "convert"
  /** The memory limit was exceeded; `peakBytes` and `limitBytes` are set. */
  | "memory"
  | "timeout"
  | "aborted"
  /** The progress callback returned false. */
  | "cancelled"
  /** The WebAssembly instance trapped for another reason (a bug: please report it). */
  | "trap"
  /** Pool only: the worker thread crashed or was killed. */
  | "worker"
  /** Pool only: the pool was closed. */
  | "closed";

export class SlidepdfError extends Error {
  code: SlidepdfErrorCode;
  peakBytes?: number;
  limitBytes?: number;
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
  /** Called after every slide. */
  onProgress?: (done: number, total: number) => void;
  /** Polled after every slide; returning true stops with code "cancelled". */
  shouldCancel?: () => boolean;
}

export interface StreamResult {
  manifest: Manifest;
  manifestJson: string;
  report: Report;
  stats: ConversionStats;
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
  stats: ConversionStats;
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
export function convertFile(input: string, outDir: string, options?: ConvertOptions & { onProgress?: (done: number, total: number) => void }): StreamResult;

/** Sizes up a deck in milliseconds without converting it, to decide its queue or refuse it. */
export function inspect(pptx: ByteSource | Bytes): Inspection;

/** Node only: `inspect` on a .pptx file. */
export function inspectFile(input: string): Inspection;

/**
 * Caps the heap of this WebAssembly instance at `mb` megabytes (0 removes
 * the cap). A conversion that exceeds it fails with code "memory". In
 * Node the instance is reloaded afterwards; in browsers and Workers it is
 * dead, so only set a limit where the isolate can be thrown away.
 */
export function setMemoryLimit(mb: number): void;

/** Heap bytes in use, the peak of the last conversion, the cap, and the size of WebAssembly memory (which only grows). */
export function memoryStats(): { inUse: number; peak: number; limit: number; linear: number };

export interface JobOptions {
  /** Overrides the pool's timeoutMs for this job. */
  timeoutMs?: number;
  /** Overrides the pool's memoryMb for this job. */
  memoryMb?: number;
  signal?: AbortSignal;
  onProgress?: (done: number, total: number) => void;
}

export interface PoolOptions {
  /** Worker threads, so also the number of conversions in flight. Default 1. */
  size?: number;
  /** Default timeout per job; 0 (default) is none. */
  timeoutMs?: number;
  /** Default heap cap per job in MB; 0 (default) is none. */
  memoryMb?: number;
  /**
   * Replace a worker once its WebAssembly memory passed this (MB). Memory
   * never shrinks inside a worker, so this bounds what idle workers keep.
   * A replacement costs about 15 ms. Default 64.
   */
  recycleAboveMb?: number;
  /** After a timeout or abort, how long a worker may take to stop on its own before it is killed. Default 5000. */
  killGraceMs?: number;
  /** Compile the whole module up front so the first conversion is as fast as the rest (sets a process-wide V8 flag). Default true. */
  eagerCompile?: boolean;
}

export interface PoolStats {
  size: number;
  workers: number;
  busy: number;
  queued: number;
  /** Highest WebAssembly memory (MB) each live worker reached. */
  highMb: number[];
}

/** Node only: worker threads with a memory cap, a timeout and cancellation per job. See the README's "Running in pods". */
export interface Pool {
  convertFile(input: string, outDir: string, options?: ConvertOptions & JobOptions): Promise<StreamResult>;
  inspectFile(input: string, options?: JobOptions): Promise<Inspection>;
  assembleFiles(inputs: string[], outPath: string, options?: JobOptions): Promise<number>;
  /** Spawns all workers now. */
  warmUp(): void;
  stats(): PoolStats;
  close(): Promise<void>;
}

/** Node only. Jobs beyond `size` wait in a queue; the process never holds more than `size` conversions in memory. */
export function createPool(options?: PoolOptions): Pool;

/** Node only: one conversion in its own worker thread, whose memory is released when it ends. */
export function convertFileInWorker(input: string, outDir: string, options?: ConvertOptions & JobOptions): Promise<StreamResult>;

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

/** Texts of the viewer; `{message}` and `{number}` are filled in. */
export interface SlideViewerLabels {
  loading: string;
  openError: string;
  slideError: string;
  prev: string;
  next: string;
  search: string;
  searchLabel: string;
  focus: string;
  focusTitle: string;
  fullscreen: string;
  fullscreenTitle: string;
  slides: string;
  slide: string;
}

/**
 * Fetches one file for the viewer: the manifest (`kind` "manifest") or a
 * fragment ("slide"). `url` is already resolved against the manifest URL.
 */
export type SlideViewerLoader = (
  url: string,
  kind: "manifest" | "slide",
) => Promise<Response | ArrayBuffer | Uint8Array | Blob | Manifest>;

/** Properties of the `<slide-viewer>` element, set from JavaScript. */
export interface SlideViewerElement extends HTMLElement {
  /** fetch() options for the manifest and every fragment (headers, credentials). */
  requestInit?: RequestInit;
  /** Fetches every file itself; takes precedence over `requestInit`. */
  loader?: SlideViewerLoader;
  /** A pdf.js module (3.x or 4.x) with its worker set up, instead of the CDN build. */
  pdfjsLib?: unknown;
  /** Overrides some or all built-in texts (English, or Spanish with lang="es"). */
  labels?: Partial<SlideViewerLabels>;
  readonly count: number;
  next(): void;
  prev(): void;
  goTo(number: number): void;
  toggleFocus(): void;
  toggleFullscreen(): void;
  search(text: string): number[];
}
