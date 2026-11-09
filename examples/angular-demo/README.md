# slidepdf Angular demo

Angular 22 page plus a small Node (TypeScript) API that uses the local
`slidepdf` npm package.

- `server/server.ts`: `POST /api/convert` converts an uploaded PPTX and stores
  `deck.pdf`, `manifest.json` and one PDF per slide under `store/<id>/` (the
  stand-in for a CDN bucket). `GET /api/decks/<id>/assemble?range=2-4` rebuilds
  a PDF from the stored fragments after checking their hashes.
- `server/dedupe.ts`: identical decks are converted once (see
  [Deduplicated conversions](#deduplicated-conversions)).
- `GET /metrics`: Prometheus text format (conversions, durations, workers,
  queue, cache).
- `src/app`: upload form, fidelity report, `<slide-viewer>` with three themes
  (built-in, CSS variables, `::part()` + slots), course search and the
  tracking events log.

## Run

Build the npm package first (from the repository root): `scripts/build-npm.sh`.

```sh
npm install
npm run api        # http://localhost:3001  (Node 23+, runs the .ts directly)
npx ng serve       # http://localhost:4200, proxies /api and /store to the API
```

## Deduplicated conversions

`POST /api/convert` hashes the upload (SHA-256, while it streams to disk)
before converting, and answers with the header `X-Slidepdf-Cache`:

| value    | meaning |
|----------|---------|
| `hit`    | The same deck with the same `preset` was converted before; its result came from the on-disk cache, so nothing was converted (`sizes.ms` is 0). |
| `joined` | The same deck and preset was being converted right now; this request attached to that job instead of starting another. |
| `miss`   | A new conversion ran for this request. |

Every request still gets its own deck folder (`id`) in `store/`, copied from
the cached result, and its own `manifest.source` (the `name` it sent). The
header is also set on conversion errors that happen after the upload (for
example a `422` for a file that is not a PPTX); failures are never cached.

If several requests share a running job and one disconnects, the job goes on
for the others; it is cancelled only when the last one leaves.

The cache is a least-recently-used folder of finished results, keyed by hash
and preset, and survives restarts. Run one server per `CACHE_DIR`.

| variable    | default                            | meaning |
|-------------|------------------------------------|---------|
| `CACHE_MB`  | `256`                              | Size limit of the result cache in MB. Oldest unused entries go first; an entry in use is never removed, so the folder can briefly pass the limit. |
| `CACHE_DIR` | `<os.tmpdir()>/slidepdf-demo-cache` | Where results (and the uploads being converted) are kept. |

`/metrics` adds `slidepdf_cache_total{result="hit|miss|joined"}` (counter) and
`slidepdf_cache_bytes` (gauge, bytes held by the cache).

An unknown `preset` is now refused with `400` instead of reaching the converter.

## Tests

```sh
node --test server/server.test.ts
```

The end-to-end tests need the WebAssembly build (`npm/dist`, see Run above) and
skip without it; the cache, job and metrics unit tests always run.
