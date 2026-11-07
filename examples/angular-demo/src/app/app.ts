import { Component, CUSTOM_ELEMENTS_SCHEMA, computed, signal } from '@angular/core';
import 'slidepdf/viewer';
import type { Manifest } from 'slidepdf';

interface ConvertResponse {
  id: string;
  manifest: Manifest;
  report: { clean: boolean; text: string; perSlide: { slide: number; warnings: string[] }[]; general: string[] };
  sizes: { pptx: number; pdf: number; fragments: number; ms: number };
}

type Theme = 'default' | 'light' | 'brand';

@Component({
  selector: 'app-root',
  templateUrl: './app.html',
  styleUrl: './app.css',
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
})
export class App {
  protected readonly preset = signal<'light' | 'balanced' | 'print'>('balanced');
  protected readonly theme = signal<Theme>('default');
  protected readonly busy = signal(false);
  protected readonly error = signal<string | null>(null);
  protected readonly result = signal<ConvertResponse | null>(null);
  protected readonly events = signal<string[]>([]);
  protected readonly range = signal('');
  protected readonly query = signal('');

  protected readonly manifestUrl = computed(() => {
    const r = this.result();
    return r ? `/store/${r.id}/manifest.json` : null;
  });

  protected readonly hits = computed(() => {
    const r = this.result();
    const q = fold(this.query()).split(/\s+/).filter(Boolean);
    if (!r || !q.length) return [];
    return r.manifest.slides.filter((s) => {
      const hay = fold(`${s.title ?? ''} ${s.text}`);
      return q.every((w) => hay.includes(w));
    });
  });

  protected readonly assembleUrl = computed(() => {
    const r = this.result();
    if (!r) return null;
    const range = this.range().trim();
    return `/api/decks/${r.id}/assemble${range ? `?range=${encodeURIComponent(range)}` : ''}`;
  });

  protected async upload(input: HTMLInputElement) {
    const file = input.files?.[0];
    if (!file) return;
    this.busy.set(true);
    this.error.set(null);
    this.result.set(null);
    this.events.set([]);
    try {
      const res = await fetch(`/api/convert?preset=${this.preset()}&name=${encodeURIComponent(file.name)}`, {
        method: 'POST',
        body: file,
      });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error ?? `HTTP ${res.status}`);
      this.result.set(data);
    } catch (e) {
      this.error.set((e as Error).message);
    } finally {
      this.busy.set(false);
      input.value = '';
    }
  }

  protected log(type: string, e: Event) {
    const d = (e as CustomEvent).detail;
    this.events.update((list) => [`${new Date().toLocaleTimeString()}  ${type}  ${JSON.stringify(d)}`, ...list].slice(0, 50));
  }

  protected goTo(viewer: HTMLElement, n: number) {
    (viewer as HTMLElement & { goTo(n: number): void }).goTo(n);
  }

  protected kb(n: number) {
    return `${(n / 1024).toFixed(1)} KB`;
  }
}

function fold(s: string) {
  return s.normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase();
}
