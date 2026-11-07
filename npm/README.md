# slidepdf

Ultra-light PPTX to PDF converter (Rust compiled to WebAssembly) with
per-slide PDF fragments, reassembly, a fidelity report and a
`<slide-viewer>` web component.

```sh
npm install slidepdf
```

```ts
// Node: bounded memory, writes slide-NNN.pdf, deck.pdf, manifest.json, report.json
import { convertFile } from "slidepdf";
const { manifest, report } = convertFile("course.pptx", "out/course-1", { preset: "balanced" });
console.log(report.text);
```

```ts
// Any runtime
import { ready, convert, assemble, search } from "slidepdf"; // or "slidepdf/lite"
await ready();
const out = convert(pptxBytes, { preset: "light" });
```

```html
<script type="module">import "slidepdf/viewer";</script>
<slide-viewer manifest="/cdn/course-1/manifest.json"></slide-viewer>
```

Full documentation: https://github.com/Rixmerz/slidepdf

Licensed under MIT or Apache-2.0.
