# slidepdf Angular demo

Angular 22 page plus a small Node (TypeScript) API that uses the local
`slidepdf` npm package.

- `server/server.ts`: `POST /api/convert` converts an uploaded PPTX and stores
  `deck.pdf`, `manifest.json` and one PDF per slide under `store/<id>/` (the
  stand-in for a CDN bucket). `GET /api/decks/<id>/assemble?range=2-4` rebuilds
  a PDF from the stored fragments after checking their hashes.
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
