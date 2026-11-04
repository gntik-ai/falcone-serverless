# Compile/cache fixtures

`valid.wasm` is the eight-byte empty component. `invalid.wasm` has component
magic followed by a malformed section. `native.elf` is rejected at intake.
The precompiled-input case generates real native output with the locked engine
in the test, avoiding a checked-in architecture-specific engine artifact.

The qualification fixture encoder expands `cpu-heavy.wat` to 128 functions with
8192 multiply/add pairs each and `memory-heavy.wat` to one function with 131072
pairs. An independent WAT parser checks their exact bytes. Neither workload is
instantiated or executed. CPU failure uses 1 s / 1 GiB / 300 s; memory failure
uses 60 s / 128 MiB / 300 s. Both controls use 120 s / 4 GiB / 300 s.

These are qualification budgets, not production defaults. The operator builds
and pins the image containing the qualification runner for the later cluster
evidence run on #52.
