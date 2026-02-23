# Lua Plugin Architecture & Ratecontroller Trait — Implementation Tasks

## Design decisions

- Rust owns the main loop for all ratecontroller implementations
- Lua ratecontrollers provide a pure `calculate(data) -> results` function (no I/O)
- Plugins work uniformly with all ratecontroller types, managed by Rust
- Upstream Lua project gets a small refactor: extract calculation from the loop

---

## Phase 1: Extract the Ratecontroller trait

1. Convert `src/ratecontroller.rs` from a single file into a module directory (`src/ratecontroller/mod.rs` + `src/ratecontroller/ewma.rs`)
2. Define a `Ratecontroller` trait with a `run` method. It needs to be object-safe and thread-movable.
3. Create a `RatecontrollerContext` struct that bundles all the shared state and config needed to construct any ratecontroller (avoids repeating 7 constructor arguments everywhere)
4. Decide which types and functions from the current file are shared infrastructure vs EWMA-specific. Shared things go in `mod.rs`, EWMA-specific things go in `ewma.rs`
5. Rename the current struct to `EwmaRatecontroller`, implement the trait
6. Write a factory function that takes the context and returns a boxed trait object (only supports "ewma" for now)
7. Update `main.rs` to use the context struct and factory function
8. Remove the Lua test code from `main.rs` (lines 41–51)
9. Verify: `cargo check`, behavior unchanged

## Phase 2: Config fields

10. Add a `rate_controller` field to `Config` — string, defaults to `"ewma"`, loaded from env/UCI like the other fields
11. Add a `plugin_scripts` field to `Config` — needs to support multiple paths (think about how to represent a list in a single env var / UCI value)
12. Wire the factory function to use the new `rate_controller` config field
13. Verify: `cargo check`

## Phase 3: Plugin system

14. Define a `Readings` struct that matches the upstream Lua readings table format (the fields plugins receive each iteration)
15. Define a `PluginResults` struct for the optional overrides plugins can return (next rates, delay thresholds)
16. Build a `PluginManager` that owns a Lua state, loads plugin scripts at startup, and calls their `process` functions each iteration
17. Think about error handling: what happens when a plugin fails to load? When `process()` throws a Lua error? When it returns unexpected types?
18. Integrate the `PluginManager` into `EwmaRatecontroller` — it should be optional (only created when plugins are configured)
19. Find the right hook point in the EWMA `run()` loop to call plugins — after rate calculation, before applying to qdisc
20. Handle the delay threshold overrides (`ul_max_delta_owd`, `dl_max_delta_owd`) — these need to persist across iterations and feed back into the next `calculate_rate()` call
21. Write a table conversion helper to go from `Readings` struct → Lua table
22. Write a table extraction helper to go from Lua result table → `PluginResults` struct
23. Verify: `cargo check`, test with a trivial Lua plugin that just returns an empty table

## Phase 4: LuaRatecontroller

24. Extract the OWD delta collection logic from `EwmaRatecontroller::update_deltas()` into a standalone function in `mod.rs` that both implementations can call. It should return the raw sorted delta vectors.
25. Refactor `EwmaRatecontroller::update_deltas()` to use the new shared function
26. Build the `LuaRatecontroller` struct — think about what state it needs to own (the Lua state, qdisc handles, shared Arc pointers, plugin manager)
27. In the constructor: create the Lua state, load and validate the script, call `configure(settings)` if the script provides it
28. Write the `run()` loop — it follows the same structure as the EWMA loop (sleep, read stats, collect deltas, handle empty deltas), but delegates the rate calculation step to a Lua function call
29. Build the data table that gets passed to the Lua `calculate()` function — think about what information the Lua script needs to replicate the EWMA algorithm (current rates, deltas, byte counts, duration, config values)
30. Handle Lua errors per-iteration: if `calculate()` fails, log and fall back to minimum rates for that tick, don't crash the loop
31. Call plugins after Lua returns, same as the EWMA path
32. Wire the factory function to create `LuaRatecontroller` when `rate_controller` is a file path instead of `"ewma"`
33. Verify: `cargo check`, test with a minimal Lua script that returns fixed rates

## Phase 5: Lua bridge helpers

34. Consolidate the Rust↔Lua conversion helpers (readings→table, config→table, vec→table, table→results) into a shared module that both `PluginManager` and `LuaRatecontroller` use
35. Verify: `cargo check`, `cargo clippy`

---

## Things to keep in mind

- `mlua::Lua` is `Send` but not `Sync` — fine for single-threaded ownership, but can't be shared across threads
- The existing `PingListener`/`PingSender` traits in `pinger.rs` use `Box<dyn Trait + Send>` — same pattern applies here
- Plugin errors should never crash the program. Ratecontroller script load failure *should* be fatal.
- The `Readings` struct fields should match upstream naming so existing Lua plugins port with minimal changes
