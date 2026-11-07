// <slide-viewer> - lazy viewer for slidepdf fragments.
//
//   <script type="module" src="slide-viewer.js"></script>
//   <slide-viewer manifest="manifest.json"></slide-viewer>
//
// Attributes:
//   manifest       URL of manifest.json (fragment URLs resolve against it)
//   start          1-based slide to open first
//   pdfjs          base URL of a pdf.js 3.x build (default: cdnjs)
//   show-warnings  show the fidelity warnings of the current slide
//   view-threshold ms on screen before a slide counts as viewed (default 2000)
//
// Events (all bubble, details in `event.detail`):
//   ready        { count }
//   slidechange  { number, previous }
//   slideviewed  { number, ms }            fired when leaving a slide
//   completed    { total, ms }             every slide viewed at least once
//
// Methods: next(), prev(), goTo(number), toggleFocus(), toggleFullscreen(),
//          search(text) -> [numbers]
//
// Styling, from simplest to most control:
//   1. CSS variables on the element (or any ancestor):
//        --sv-bg --sv-fg --sv-muted --sv-accent --sv-panel --sv-border
//        --sv-radius --sv-button-radius --sv-button-bg --sv-button-fg
//        --sv-nav-bg --sv-nav-size --sv-thumb-width --sv-font --sv-page-shadow
//   2. ::part() selectors, one per piece:
//        frame stage page nav nav-prev nav-next bar button button-prev
//        button-next button-focus button-fullscreen counter title search
//        warning thumbs thumb thumb-current thumb-seen thumb-number
//      e.g.  slide-viewer::part(button) { border-radius: 999px; }
//   3. Slots to replace icons or add controls:
//        prev-icon next-icon focus-icon fullscreen-icon bar-start bar-end
//   4. Attribute `unstyled` drops the built-in look except layout.

const DEFAULT_PDFJS = "https://cdnjs.cloudflare.com/ajax/libs/pdf.js/3.11.174";

let pdfjsPromise = null;
function loadPdfJs(base) {
  if (window.pdfjsLib) return Promise.resolve(window.pdfjsLib);
  if (!pdfjsPromise) {
    pdfjsPromise = new Promise((resolve, reject) => {
      const s = document.createElement("script");
      s.src = `${base}/pdf.min.js`;
      s.onload = () => {
        window.pdfjsLib.GlobalWorkerOptions.workerSrc = `${base}/pdf.worker.min.js`;
        resolve(window.pdfjsLib);
      };
      s.onerror = () => reject(new Error("could not load pdf.js from " + base));
      document.head.appendChild(s);
    });
  }
  return pdfjsPromise;
}

// Accent- and case-insensitive text for search.
const fold = (s) => (s || "").normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

const CSS = `
:host { display: block; position: relative; font: var(--sv-font, 14px/1.4 system-ui, sans-serif);
  background: var(--sv-bg, #1d1f23); color: var(--sv-fg, #f2f2f2);
  border-radius: var(--sv-radius, 8px); overflow: hidden; }
:host([unstyled]) { background: none; color: inherit; border-radius: 0; }
:host([unstyled]) .bar, :host([unstyled]) .thumbs { background: none; }
:host([unstyled]) button, :host([unstyled]) input { all: revert; }
:host(:fullscreen) { border-radius: 0; }
.wrap { display: flex; flex-direction: column; height: 100%; min-height: 320px; }
.stage { position: relative; flex: 1; display: flex; align-items: center;
  justify-content: center; padding: 12px; min-height: 0; touch-action: pan-y; }
.page { position: relative; box-shadow: var(--sv-page-shadow, 0 2px 12px rgba(0,0,0,.45)); background: #fff; }
.page canvas { display: block; width: 100%; height: 100%; }
.loading { position: absolute; inset: 0; display: grid; place-items: center;
  color: #666; font-size: 13px; }
.nav { position: absolute; top: 50%; transform: translateY(-50%); width: var(--sv-nav-size, 44px);
  height: var(--sv-nav-size, 44px); border-radius: 50%; border: 0; background: var(--sv-nav-bg, rgba(0,0,0,.45));
  color: #fff; font-size: 22px;
  cursor: pointer; display: grid; place-items: center; }
.nav:hover { background: rgba(0,0,0,.7); }
.nav[disabled] { opacity: .25; cursor: default; }
.nav.prev { left: 8px; } .nav.next { right: 8px; }
.bar { display: flex; align-items: center; gap: 8px; padding: 6px 10px; background: var(--sv-panel, #2a2d33); }
.bar button { background: var(--sv-button-bg, transparent); color: var(--sv-button-fg, var(--sv-fg, #f2f2f2));
  border: 1px solid var(--sv-border, #444a52); border-radius: var(--sv-button-radius, 6px); padding: 4px 10px; cursor: pointer; font: inherit; min-height: 32px; }
.bar button:hover { border-color: var(--sv-accent, #4c8dff); }
.count { font-variant-numeric: tabular-nums; min-width: 64px; text-align: center; }
.title { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--sv-muted, #9aa0a6); }
.bar input { background: var(--sv-input-bg, #1b1d21); color: var(--sv-fg, #f2f2f2); border: 1px solid var(--sv-border, #444a52);
  border-radius: 6px; padding: 5px 8px; width: 150px; font: inherit; }
.warn { color: #ffb74d; cursor: help; display: none; }
.thumbs { display: flex; gap: 8px; overflow-x: auto; padding: 8px 10px; background: var(--sv-panel, #2a2d33);
  border-top: 1px solid #3a3e45; scroll-behavior: smooth; }
.thumb { flex: 0 0 auto; width: var(--sv-thumb-width, 120px); cursor: pointer; border: 2px solid transparent;
  border-radius: 4px; background: #fff; position: relative; padding: 0; }
.thumb canvas { display: block; width: 100%; height: 100%; border-radius: 2px; }
.thumb span { position: absolute; left: 3px; bottom: 2px; font-size: 11px; color: #fff;
  background: rgba(0,0,0,.6); padding: 0 4px; border-radius: 3px; }
.thumb.current { border-color: var(--sv-accent, #4c8dff); }
.thumb.dim { opacity: .25; }
.thumb.seen::after { content: ""; position: absolute; right: 4px; top: 4px; width: 7px; height: 7px;
  border-radius: 50%; background: #3ecf8e; }
:host([focus]) .thumbs, :host(:fullscreen) .thumbs { display: none; }
:host(:fullscreen) .stage { padding: 0; }
.error { padding: 24px; color: #ff8a80; }
@media (max-width: 600px) { .bar input { width: 90px; } .title { display: none; } }
`;

class SlideViewer extends HTMLElement {
  static get observedAttributes() { return ["manifest"]; }

  constructor() {
    super();
    this.attachShadow({ mode: "open" });
    this.manifest = null;
    this.current = 0;
    this._docs = new Map();
    this._seen = new Set();
    this._enteredAt = 0;
    this._startedAt = 0;
    this._completed = false;
    this._renderToken = 0;
  }

  connectedCallback() {
    this.shadowRoot.innerHTML = `<style>${CSS}</style>
      <div class="wrap" part="frame">
        <div class="stage" part="stage">
          <div class="page" part="page"><canvas></canvas><div class="loading">Loading...</div></div>
          <button class="nav prev" part="nav nav-prev" aria-label="Previous slide"><slot name="prev-icon">&#8249;</slot></button>
          <button class="nav next" part="nav nav-next" aria-label="Next slide"><slot name="next-icon">&#8250;</slot></button>
        </div>
        <div class="bar" part="bar">
          <slot name="bar-start"></slot>
          <button data-act="prev" part="button button-prev" aria-label="Previous slide">&#8592;</button>
          <span class="count" part="counter" aria-live="polite">- / -</span>
          <button data-act="next" part="button button-next" aria-label="Next slide">&#8594;</button>
          <span class="warn" part="warning" title="">&#9888;</span>
          <span class="title" part="title"></span>
          <input type="search" part="search" placeholder="Search" aria-label="Search slides">
          <button data-act="focus" part="button button-focus" title="Large slide (hide thumbnails)" aria-label="Large slide"><slot name="focus-icon">&#9635;</slot></button>
          <button data-act="fullscreen" part="button button-fullscreen" title="Fullscreen (F)" aria-label="Fullscreen"><slot name="fullscreen-icon">&#x26F6;</slot></button>
          <slot name="bar-end"></slot>
        </div>
        <div class="thumbs" part="thumbs" role="listbox" aria-label="Slides"></div>
      </div>`;
    const $ = (s) => this.shadowRoot.querySelector(s);
    this._els = { stage: $(".stage"), page: $(".page"), canvas: $(".page canvas"), loading: $(".loading"),
      count: $(".count"), title: $(".title"), thumbs: $(".thumbs"), warn: $(".warn"), search: $("input") };
    $(".nav.prev").onclick = () => this.prev();
    $(".nav.next").onclick = () => this.next();
    this.shadowRoot.querySelectorAll(".bar button").forEach((b) => {
      b.onclick = () => ({ prev: () => this.prev(), next: () => this.next(),
        focus: () => this.toggleFocus(), fullscreen: () => this.toggleFullscreen() })[b.dataset.act]();
    });
    this._els.search.oninput = () => this._applySearch(this._els.search.value);
    this._els.search.onkeydown = (e) => {
      if (e.key === "Enter") { const hits = this.search(this._els.search.value); if (hits.length) this.goTo(hits.find((n) => n > this.current + 1) || hits[0]); }
      e.stopPropagation();
    };
    if (!this.hasAttribute("tabindex")) this.tabIndex = 0;
    this.addEventListener("keydown", (e) => this._onKey(e));
    this._onResize = () => this._renderCurrent();
    this._ro = new ResizeObserver(() => { clearTimeout(this._rt); this._rt = setTimeout(this._onResize, 80); });
    this._ro.observe(this._els.stage);
    this._onHide = () => { if (document.visibilityState === "hidden") this._leave(); else this._enteredAt = performance.now(); };
    document.addEventListener("visibilitychange", this._onHide);
    this._wireSwipe();
    if (this.getAttribute("manifest")) this._load();
  }

  disconnectedCallback() {
    this._leave();
    this._ro && this._ro.disconnect();
    document.removeEventListener("visibilitychange", this._onHide);
    this._docs.forEach((p) => p.then((d) => d.destroy()).catch(() => {}));
    this._docs.clear();
  }

  attributeChangedCallback(name, oldV, newV) {
    if (name === "manifest" && oldV !== newV && this._els) this._load();
  }

  get count() { return this.manifest ? this.manifest.slides.length : 0; }

  async _load() {
    // A new deck: report the slide being left, then drop everything that
    // belongs to the old one (open documents are keyed by slide index).
    this._leave();
    this._docs.forEach((p) => p.then((d) => d.destroy()).catch(() => {}));
    this._docs.clear();
    this.current = 0;
    this._renderToken++;
    this.manifest = null;
    // Only the latest load may finish: a slower earlier fetch is dropped.
    const load = (this._loadId = (this._loadId || 0) + 1);
    const url = new URL(this.getAttribute("manifest"), document.baseURI);
    try {
      const [lib, res] = await Promise.all([loadPdfJs(this.getAttribute("pdfjs") || DEFAULT_PDFJS), fetch(url)]);
      if (load !== this._loadId) return;
      if (!res.ok) throw new Error(`manifest: HTTP ${res.status}`);
      this._lib = lib;
      const manifest = await res.json();
      if (load !== this._loadId) return;
      this.manifest = manifest;
      this._base = url;
      this._seen.clear();
      this._completed = false;
      this._startedAt = performance.now();
      const ratio = this.manifest.height / this.manifest.width;
      this._ratio = ratio;
      this._buildThumbs();
      const start = Math.min(Math.max(parseInt(this.getAttribute("start") || "1", 10) || 1, 1), this.count);
      this._emit("ready", { count: this.count });
      this.goTo(start, true);
    } catch (e) {
      if (load !== this._loadId) return;
      // textContent, not innerHTML: messages can echo server or manifest data.
      this._els.loading.style.display = "grid";
      this._els.loading.textContent = `Could not open the presentation: ${e.message}`;
    }
  }

  _doc(i) {
    if (!this._docs.has(i)) {
      const src = new URL(this.manifest.slides[i].file, this._base).href;
      const p = this._lib.getDocument({ url: src, disableAutoFetch: true }).promise;
      p.catch(() => this._docs.delete(i));
      this._docs.set(i, p);
    }
    return this._docs.get(i);
  }

  // Keeps only the current slide and its neighbours open; pdf.js documents
  // hold decoded images, so an unbounded cache exhausts memory on phones.
  _evict() {
    for (const [i, p] of this._docs) {
      if (Math.abs(i - this.current) > 2) {
        this._docs.delete(i);
        p.then((d) => d.destroy()).catch(() => {});
      }
    }
  }

  // Thumbnails open their fragment, draw it small and close it right away.
  async _drawThumb(i, canvas, cssWidth) {
    if (this._docs.has(i)) return this._draw(i, canvas, cssWidth);
    const src = new URL(this.manifest.slides[i].file, this._base).href;
    const doc = await this._lib.getDocument({ url: src, disableAutoFetch: true }).promise;
    try {
      await this._render(doc, canvas, cssWidth);
    } finally {
      doc.destroy();
    }
  }

  async _draw(i, canvas, cssWidth) {
    return this._render(await this._doc(i), canvas, cssWidth);
  }

  async _render(doc, canvas, cssWidth) {
    const page = await doc.getPage(1);
    const base = page.getViewport({ scale: 1 });
    const dpr = Math.min(window.devicePixelRatio || 1, 3);
    const vp = page.getViewport({ scale: (cssWidth / base.width) * dpr });
    const tmp = document.createElement("canvas");
    tmp.width = Math.ceil(vp.width);
    tmp.height = Math.ceil(vp.height);
    await page.render({ canvasContext: tmp.getContext("2d"), viewport: vp }).promise;
    canvas.width = tmp.width;
    canvas.height = tmp.height;
    canvas.getContext("2d").drawImage(tmp, 0, 0);
  }

  _fit() {
    const st = this._els.stage;
    const pad = this.matches(":fullscreen") ? 0 : 24;
    const w = Math.max(st.clientWidth - pad, 50), h = Math.max(st.clientHeight - pad, 50);
    const width = Math.min(w, h / this._ratio);
    this._els.page.style.width = `${width}px`;
    this._els.page.style.height = `${width * this._ratio}px`;
    return width;
  }

  async _renderCurrent() {
    if (!this.manifest) return;
    const token = ++this._renderToken;
    const width = this._fit();
    this._els.loading.style.display = "grid";
    try {
      const canvas = document.createElement("canvas");
      await this._draw(this.current, canvas, width);
      if (token !== this._renderToken) return;
      this._els.canvas.replaceWith(canvas);
      this._els.canvas = canvas;
      this._els.loading.style.display = "none";
    } catch {
      if (token === this._renderToken) this._els.loading.textContent = "Could not load this slide";
    }
    // Prefetch neighbours so the next click is instant, and drop the rest.
    [this.current + 1, this.current - 1].filter((j) => j >= 0 && j < this.count).forEach((j) => this._doc(j));
    this._evict();
  }

  _buildThumbs() {
    const box = this._els.thumbs;
    box.innerHTML = "";
    this._io && this._io.disconnect();
    this._io = new IntersectionObserver((entries) => {
      entries.forEach((en) => {
        if (!en.isIntersecting) return;
        const t = en.target;
        this._io.unobserve(t);
        this._drawThumb(+t.dataset.i, t.querySelector("canvas"), 120).catch(() => {});
      });
    }, { root: box, rootMargin: "0px 300px" });
    this.manifest.slides.forEach((s, i) => {
      const b = document.createElement("button");
      b.className = "thumb";
      b.dataset.i = i;
      b.setAttribute("role", "option");
      b.setAttribute("aria-label", `Slide ${s.number}${s.title ? ": " + s.title : ""}`);
      b.style.height = `${120 * this._ratio}px`;
      b.setAttribute("part", "thumb");
      const num = document.createElement("span");
      num.setAttribute("part", "thumb-number");
      num.textContent = String(s.number);
      b.append(document.createElement("canvas"), num);
      b.onclick = () => this.goTo(i + 1);
      box.appendChild(b);
      this._io.observe(b);
    });
  }

  goTo(number, initial = false) {
    if (!this.manifest) return;
    const i = Math.min(Math.max(number - 1, 0), this.count - 1);
    if (i === this.current && !initial) return;
    const previous = initial ? null : this.current + 1;
    if (!initial) this._leave();
    this.current = i;
    this._enteredAt = performance.now();
    const s = this.manifest.slides[i];
    this._els.count.textContent = `${i + 1} / ${this.count}`;
    this._els.title.textContent = s.title || "";
    const warn = this.hasAttribute("show-warnings") && s.warnings && s.warnings.length;
    this._els.warn.style.display = warn ? "inline" : "none";
    this._els.warn.title = warn ? s.warnings.join("\n") : "";
    this.shadowRoot.querySelectorAll(".nav.prev, [data-act=prev]").forEach((b) => (b.disabled = i === 0));
    this.shadowRoot.querySelectorAll(".nav.next, [data-act=next]").forEach((b) => (b.disabled = i === this.count - 1));
    this._els.thumbs.querySelectorAll(".thumb").forEach((t, j) => {
      t.classList.toggle("current", j === i);
      t.setAttribute("part", ["thumb", j === i && "thumb-current", t.classList.contains("seen") && "thumb-seen"].filter(Boolean).join(" "));
      t.setAttribute("aria-selected", j === i);
    });
    const cur = this._els.thumbs.children[i];
    if (cur) cur.scrollIntoView({ block: "nearest", inline: "center" });
    this._renderCurrent();
    this._emit("slidechange", { number: i + 1, previous });
  }

  next() { this.goTo(this.current + 2); }
  prev() { this.goTo(this.current); }

  toggleFocus() { this.toggleAttribute("focus"); requestAnimationFrame(() => this._renderCurrent()); }

  toggleFullscreen() {
    if (document.fullscreenElement === this) document.exitFullscreen();
    else if (this.requestFullscreen) this.requestFullscreen().catch(() => this.toggleFocus());
    else this.toggleFocus();
  }

  search(text) {
    const q = fold(text).split(/\s+/).filter(Boolean);
    if (!q.length || !this.manifest) return [];
    return this.manifest.slides
      .filter((s) => { const hay = fold(`${s.title || ""} ${s.text || ""}`); return q.every((w) => hay.includes(w)); })
      .map((s) => s.number);
  }

  _applySearch(text) {
    const hits = new Set(this.search(text));
    const active = fold(text).trim().length > 0;
    this._els.thumbs.querySelectorAll(".thumb").forEach((t, j) => t.classList.toggle("dim", active && !hits.has(j + 1)));
  }

  // A slide counts as viewed once it stayed on screen for view-threshold ms.
  _leave() {
    if (!this.manifest || !this._enteredAt) return;
    const ms = Math.round(performance.now() - this._enteredAt);
    this._enteredAt = 0;
    const number = this.current + 1;
    const threshold = parseInt(this.getAttribute("view-threshold") || "2000", 10);
    this._emit("slideviewed", { number, ms });
    if (ms >= threshold) {
      this._seen.add(number);
      const t = this._els.thumbs.children[this.current];
      if (t) { t.classList.add("seen"); t.part.add("thumb-seen"); }
    }
    if (!this._completed && this._seen.size === this.count) {
      this._completed = true;
      this._emit("completed", { total: this.count, ms: Math.round(performance.now() - this._startedAt) });
    }
  }

  _onKey(e) {
    const k = e.key;
    if (["ArrowRight", "PageDown", " "].includes(k)) this.next();
    else if (["ArrowLeft", "PageUp"].includes(k)) this.prev();
    else if (k === "Home") this.goTo(1);
    else if (k === "End") this.goTo(this.count);
    else if (k === "f" || k === "F") this.toggleFullscreen();
    else return;
    e.preventDefault();
  }

  _wireSwipe() {
    let x0 = null;
    const st = this._els.stage;
    st.addEventListener("touchstart", (e) => { x0 = e.touches[0].clientX; }, { passive: true });
    st.addEventListener("touchend", (e) => {
      if (x0 === null) return;
      const dx = e.changedTouches[0].clientX - x0;
      x0 = null;
      if (Math.abs(dx) > 50) (dx < 0 ? this.next() : this.prev());
    });
  }

  _emit(type, detail) { this.dispatchEvent(new CustomEvent(type, { detail, bubbles: true, composed: true })); }
}

if (!customElements.get("slide-viewer")) customElements.define("slide-viewer", SlideViewer);
export { SlideViewer };
