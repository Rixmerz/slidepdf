# Contributing

Thanks for helping. Issues and pull requests are welcome.

## Development

```sh
cargo test                 # unit, robustness and round-trip tests
cargo clippy --all-targets
cargo fmt
scripts/build-npm.sh       # WebAssembly package (needs the wasm32-unknown-unknown
                           # target and wasm-bindgen-cli 0.2.130)
cd npm && SLIDEPDF_TEST_PPTX=../tests/fixtures/deck.pptx npm test
```

`scripts/bench.sh` measures time, peak memory and output size on generated
decks; please run it when a change can affect memory or size.

## Reporting a rendering problem

Rendering bugs are much easier to fix with the deck that shows them. If you
can share it, attach the smallest `.pptx` that reproduces the problem and the
`slidepdf report` output. Never attach decks with confidential content.

## Guidelines

- Keep the core free of I/O: bytes or readers in, bytes or writers out.
- Hostile input must produce an error, never a panic; add a case to
  `tests/robustness.rs` when you touch a parser.
- Watch the size of the WebAssembly build; new dependencies need a reason.
- Code, comments, docs and commit messages are in English.

## License

By contributing you agree that your contributions are dual licensed under
MIT and Apache-2.0, like the rest of the project.
