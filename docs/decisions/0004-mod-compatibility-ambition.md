<!-- Transcribed from the planning documents at Phase 0.1. The plan is the
     working copy; this file is the record. Where they differ, this one is
     what the project decided. -->

# D4 — Mod compatibility ambition

**Status:** Deferred by design. Decided at the close of Phase 10.

**Context.** Arbitrary Fabric, Quilt and NeoForge mods cannot run on a Rust core — those loaders
rewrite `net.minecraft` bytecode and there is none. What *is* achievable is a tiered subset, and
the size of that subset is an empirical question, not an opinion.

**Decision.** Do not decide, and do not state a mod compatibility tier publicly, until the
Phase 10 spike has classified the real VanillaPlusTerra pack mod by mod and produced the
percentages. The measurement is the input; this decision consumes it.

**Consequences.**
- The release page says nothing about mod support until Phase 10 closes. Overstating this is the
  fastest way to lose credibility, because the first person to drop a Mixin mod into `mods/` and
  watch nothing happen will say so publicly.
- Phase 10 Step 3 — the Fabric API shim — is budgeted at nothing until Step 1 says how many mods
  it would actually unlock. If the answer is four, it does not get built.
