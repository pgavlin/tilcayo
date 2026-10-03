# Contributing

Contributions should preserve Tilcayo's terminal correctness, public API quality,
and Rust 1.80 minimum supported version.

Before submitting a change, run:

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --no-deps
cargo package --allow-dirty
```

Also run `git diff --check`. New public APIs must be documented; the crate denies
missing documentation and broken intra-doc links.

## Performance optimization protocol

Performance work is performed one independently measurable step at a time. Do
not combine several optimizations in one experiment or commit, even when they
affect the same path.

For each optimization:

1. **State one hypothesis.** Identify the suspected cost, the workload it
   affects, and the metric expected to improve. Define important regression
   cases before measuring.
2. **Benchmark the unchanged code.** Run the relevant focused benchmark
   immediately before editing and save its raw output as the baseline. Do not
   substitute historical results.
3. **Make one change.** Keep benchmark code, workload definitions, dependencies,
   build settings, and unrelated implementation details unchanged.
4. **Benchmark the changed code.** Use the same machine, terminal, geometry,
   toolchain, release profile, benchmark parameters, warmups, and sample count.
5. **Validate correctness.** Run the normal formatting, test, Clippy, rustdoc,
   package, and diff checks. Add tests for changed behavior or protocol output.
6. **Commit the change.** Each accepted optimization receives its own commit.
   Do not include the next optimization or an unmeasured cleanup.
7. **Present the results.** Report the hypothesis, exact commands and
   environment, before/after values, absolute and percentage differences,
   relevant tail latency, regressions, and commit identifier before starting
   another step.

If a change does not produce a repeatable improvement, revert it rather than
stacking another optimization on top. Keep a change only when another explicit
tradeoff justifies it, and document that tradeoff in the results and commit.

### Running the Kitty benchmark

Run the focused benchmark directly inside an un-multiplexed Kitty window:

```sh
cargo run --release --example benchmark -- \
  --suite focused --warmups 5 --samples 30 --output /tmp/tilcayo-before.csv
```

The focused suite exercises the complete [`KittyPresenter`](https://docs.rs/tilcayo/latest/tilcayo/kitty/struct.KittyPresenter.html)
path using verified shared memory without compression. It covers small,
scattered, quarter-frame, and full-frame damage at 1280×720, 1920×1080,
and the terminal's native pixel size. It also compares normal root-frame edits
with forced full-image replacement. Each timed sample ends at a subsequent
Kitty acknowledgement barrier, not merely at the application's output flush.

Use `--suite matrix` for the larger screen-size, transport, compression, and
direct-chunk matrix. The matrix is intended for broad regression checks rather
than the inner before/after loop. Run `--help` for all options.

The output CSV contains every raw sample, summary p50/p90/p99 rows, workload
metadata, and available environment versions. Each presentation sample is
split into application-side presentation time, barrier-command write time, and
acknowledgement wait time, alongside the original end-to-end duration. The
benchmark also records barrier-only samples before and after the workload. These
controls quantify the terminal query and event-loop latency included in every
acknowledgement; they should not be subtracted sample-by-sample as though they
were independent work.

The benchmark temporarily takes over terminal input and uses the alternate
screen. Do not type while it runs.

### Benchmark discipline

- Build benchmarked code in release mode.
- Use a real supported terminal when measuring terminal processing. Avoid tmux,
  SSH, and other intermediaries unless the intermediary is the subject of the
  benchmark.
- Record the operating system, architecture, terminal name and version,
  framebuffer dimensions, transport, compression policy, damage geometry,
  Tilcayo commit, Rust version, and benchmark command.
- Warm up before collecting measurements and retain individual samples. Report
  at least the median and a tail percentile; do not report only the fastest
  iteration.
- Run enough samples to distinguish the expected improvement from ordinary
  variance. Re-run noisy or surprising comparisons, including the baseline.
- Generate frame contents outside a timed interval unless frame generation is
  the behavior under test.
- Keep acknowledgement barriers when measuring terminal consumption. A
  successful writer flush alone does not show that the terminal read,
  composited, or displayed a frame.
- Separate application preparation, encoding or transfer, acknowledgement
  latency, and display latency where possible. State explicitly what each
  measurement includes.
- Check representative small-damage, large-damage, and full-frame cases. An
  optimization for one case must not silently impose a material regression on
  another.

Benchmark harness changes are not performance optimizations. Commit harness or
workload changes separately, validate them, and collect a fresh baseline before
using them to evaluate implementation work. Never change a benchmark and the
code it evaluates in the same optimization commit.

### Result report template

Use the following structure after each step:

```text
Hypothesis:
Change:
Commit:
Environment:
Benchmark command:

Case                  Before             After              Difference
<case>                <p50/p90/p99>      <p50/p90/p99>      <absolute, percent>

Regressions/noise:
Correctness validation:
Conclusion:
```

Include or link the raw benchmark output so another contributor can reproduce
and independently analyze the comparison.
