# Commons native blobs upgrade to 0.104.1

T311.35 / P34, 2026-11-09. This branch merges upstream
`1fbb0272561f3fe393328936c252c0b6511fcd5d` without rewriting the accepted Commons
history at `9413808562358124eae59af87caf785fb6533b45`. The original patch baseline
is upstream 0.103.1 `7a4c53a45ecd2bbbb5dd86b3639271004ae89d39`.

## Local delta disposition

| Original commits | Disposition on 0.104.1 |
| --- | --- |
| `a1e5912e6ced35a6eb369fc77f2dadfc5b7a4905` | Retain deferred import admission and lifecycle servicing. Upstream entity manager still reports Busy on a saturated per-entity inbox; it has no waiting-admission equivalent. Preserve Busy for unrelated operations. |
| `a7e7f0d7e66256dae37161212b7927327cd33942`, `f5bb96078e43d21cb514fae8f842b747e6227e05` | Retain the native tests and owning-formatter adjustments; apply them to current upstream types. |
| `95f7befd87839b6c76ec71a7b465a4c67b8801db` | Retain batch temporary-tag scope coverage and the held tag that makes the oracle meaningful. |
| `e09c4c7790325547d40bad677a8f1919bba6a946` | Retain external-ingress closure, final lifecycle restart and deferred-reader-loss regressions. Upstream shutdown improvements do not replace this accepted-work drain. |
| `93c546d8ee851b0222f8c748703fd07a3c256bd3` | Retain oneshot terminal persistence completion and panic/abort regression coverage. |
| `9413808562358124eae59af87caf785fb6533b45` | Preserve accepted formatting and test semantics. |

Upstream adds a queued-lifecycle `has_pending` accessor for the corrected idle
check. Reuse the existing Commons `has_pending_lifecycle` instead of keeping a
second narrower accessor: it accounts for persistence completions as well as
receiver handbacks. A new gated persistence test covers this interaction.
Preserve upstream's internal-ingress polling order while keeping external-close
drain. Adapt the retained GC regression to upstream's new optional protect
callback argument (`None` for the unprotected test). Adapt the retained actor
fixture to upstream's current-runtime constructor. Repair the upstream GC
handover test's omitted callback argument and report sweep failure with a test
panic directly, avoiding the diverging TestError conversion under strict lint.

Keep upstream's actual import cleanup, poisoned-entry, GC protection, shutdown,
range decoding and error-propagation fixes as implemented. Do not recreate them
in a Commons wrapper. All upstream workflow and manifest changes arrive through
the same merge; no vendor/cache edits or unrelated Iroh/Noq upgrade are added.

## Compatibility boundaries

Package version changes from 0.103.1 to 0.104.1. Consumers' `0.103` Cargo
requirements and root fork patches must be assigned updates; patching a library
workspace does not update its consumers. Known source changes include
`Hash::fmt_short` returning `impl Display`, downloader builder configuration,
GC protection callback API and import/stream error behavior. Consult the merged
upstream CHANGELOG for the complete release delta.

The native blobs ALPN is unchanged from the old fork. Upstream now rejects
noncanonical range encodings; conforming peers emit canonical encodings. This is
not a mixed-version interoperability verdict. Persistent file cleanup/poisoning
and GC behavior changed; the retained current-format pressure tests flush,
shutdown and reopen their data. Reading a pre-upgrade fixture and rollback to the
old release still need their assigned consumer acceptance; no schema migration,
rollback guarantee or release qualification is inferred from these unit tests.

The standalone producer keeps the upstream lock graph, including registry Iroh
1.4.1. Commons consumers use their existing Iroh 1.4.1 fork and Noq patches; P35
and subsequent adoption leaves qualify that exact composed graph. No consumer
pin changes or fork publication belong to this producer task.

## Reproducible native checks

The temporary AWS builder requires an exact committed rust-toolchain.toml.
Pin Rust 1.93.0 with rustfmt/clippy: the upstream crate/CI MSRV is 1.91, and 1.93
is an already installed, established stack toolchain. This does not raise the
crate's declared MSRV. No local Rust build or test is used.

Required exact-commit AWS checks are formatting, entity-manager library tests
(including deferred admission, cancellation, persistence panic/abort), native
FsActor/GC library regressions, four 20,000-import pressure/reopen integration
workloads, and strict library/integration Clippy. Additional upstream fs-store
and GC tests exercise the merged upstream behavior. Results and infrastructure
cleanup are recorded by the task owner in project-context; this document is not
a prewritten pass claim.
