# Native Rust projects in the fmn binary

`fmn studio-project` builds and hosts a trusted Cargo binary or example that
implements the Studio worker protocol. This is independent of the ordinary
Manim-compatible `fmn studio` command and does not use CPython.

```sh
fmn studio-project \
  --cargo "$(rustup which cargo)" \
  --manifest-path /absolute/path/to/project/Cargo.toml \
  --package my-scenes --bin scene-worker --scene MyScene \
  --worker-arg --worker
```

A project directory is accepted in place of its Cargo.toml. Package, target,
scene and exact Cargo executable selection are explicit. The selected program
must serve the native Studio protocol; merely implementing an offline render
entry point does not make a binary a Studio worker. Existing
`fmn-studio/examples/native_live.rs` demonstrates a compatible worker.

The command prints a loopback capability URL after building the worker and
receiving its initial frame. Keep stdin open while using the browser; closing
stdin stops the host and its worker. The worker runs with the project's working
directory and does not inherit the compiler's environment.

Browser **Restart** recompiles the chosen target and restores the committed
scene via the existing journal. Invalid source leaves the last healthy worker
available. The generic host does not attest arbitrary project/asset reads, so it
conservatively re-executes journal commands rather than assuming unknown
checkpoints are reusable.

Cargo runs locked and offline by default. `--allow-network` permits dependency
downloads; it does not disable `--locked`. The Cargo.lock must already resolve
the project. `--release`, repeated `--feature NAME`, and
`--build-timeout SECONDS` select build policy. The default compiler deadline is
300 seconds; explicit deadlines must be between 1 and 86400 seconds. Other
compiler output and executable-image bounds remain enforced by
`CargoBuildConfig`. The chosen Cargo should be the nightly toolchain executable
beside its rustc, not a shell alias.

Pass each worker-mode argument with a separate `--worker-arg VALUE`; these
arguments are never interpreted as Cargo flags or shell commands. Run
`fmn studio-project --help` to inspect the interface without starting a compiler
or opening a listener.
