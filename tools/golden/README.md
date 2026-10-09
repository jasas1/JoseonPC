# Golden files

This tool runs the analyzers of the macOS app and writes their output to
`crates/joseon-core/tests/golden/` and `crates/joseon-headphones/tests/golden/`. The Rust tests read those files and compare.
That is how parity with the macOS app is tested.

The tool needs the macOS repo. It runs on a Mac only. The Rust tests need only
the files, so they run on every platform.

Run it again when the macOS analyzers change:

```bash
ln -sfn /path/to/Joseon/Sources/JoseonCore tools/golden/Sources/JoseonCore
```

```bash
ln -sfn /path/to/Joseon/Sources/JoseonHeadphones tools/golden/Sources/JoseonHeadphones
```

```bash
swift run -c release --package-path tools/golden golden crates
```

The tool writes one file per part: `spectrum.txt`, `loudness.txt`, `stereo.txt`, `engine.txt`, `headphones.txt`. A run that changes a file
you did not mean to change shows in `git status`.

The links and the `.build` directory are not committed.
