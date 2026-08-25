<!-- Transcribed from the planning documents at Phase 0.1. The plan is the
     working copy; this file is the record. Where they differ, this one is
     what the project decided. -->

# D5 — Implementation language: Rust, with one bounded exception

**Status:** Accepted, 2026-08-21.

**Context.** Dust exists because a native server can use memory and cores in a way a JVM server
cannot. That premise is only worth anything if it holds all the way down — a Rust server that
quietly grows a Java half has the costs of both and the benefits of neither. The plan contains
one deliberate piece of Java (Phase 8's embedded JVM) and one incidental one (the `dust.jar`
launcher), and both are places where drift could start.

**Decision.** **Every part of the server is written in Rust.** Networking, world storage,
worldgen, simulation, persistence, the proxy, anti-cheat, the map, instancing, item and
enchantment tooling, the command system, configuration — all Rust, with no exceptions and no
"temporarily in another language until we get to it".

Java appears in exactly two places, both bounded and both enforced:

**1. The plugin API surface (Phase 8–9).** Bukkit, Spigot and Paper plugins are compiled Java
bytecode. Executing them requires a JVM; there is no alternative short of abandoning the plugin
ecosystem entirely. So Dust embeds one — pinned to a single thread, holding third-party plugin
code and a thin implementation of the Paper interfaces.

The rule that keeps this from becoming a Java server with a Rust accelerator:

> **No decision is made in Java, and no state of record lives in Java.** Every method body on the
> Java side either forwards across the boundary to Rust or marshals data. The Rust side is
> authoritative for the world, the entities, the players, the scheduler and the tick. Java is an
> adapter, and adapters do not hold opinions.

**2. The `dust.jar` launcher (Phase 19).** A few hundred lines that detect the platform, unpack
the matching native binary and execute it, so Dust drops into hosting panels expecting
`java -jar server.jar`. It contains nothing else and is never on a runtime path. Native binaries
ship alongside it and are the preferred way to run Dust; the jar is a compatibility affordance
for panels, not a component.

**Enforcement — because a principle nobody can fail is not a principle.**

- **The kill switch is the real guard.** With `jvm.enabled = false` in `dust.toml`, the server
  must boot and every feature except plugin loading must work: worldgen, simulation, proxy,
  anti-cheat, map, instancing, custom items, custom enchantments, all of it. This runs in CI as a
  standing test from Phase 8 onward. If disabling the JVM breaks anything other than plugins,
  game logic has leaked into Java and the build goes red.
- **A line-count ceiling on the Java tree**, checked in `just verify`. Raising it requires a
  commit that says why. A ceiling that only ever moves up with an explanation is a ratchet, and a
  ratchet is what stops slow drift.
- **No game-logic dependency may be imported by the Java side.** Checked mechanically.

**Consequences.**
- The performance premise holds under inspection rather than only in the README.
- Plugin calls are explicitly a cold path relative to simulation, and the architecture is free to
  treat them that way — batching events, and never serialising an event no plugin registered for.
- A plugin API method that would be easier to implement in Java is implemented in Rust anyway.
  This is a real, recurring cost and it is accepted deliberately.
- Anyone can verify the claim in one command: turn the JVM off and watch the server keep working.
