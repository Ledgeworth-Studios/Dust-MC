<!-- Transcribed from the planning documents at Phase 0.1. The plan is the
     working copy; this file is the record. Where they differ, this one is
     what the project decided. -->

# D3 — Protocol target: Minecraft 1.21.1 first

**Status:** Accepted, 2026-08-21.

**Context.** A first target version has to be chosen. It determines the mod and plugin ecosystem
Dust can interoperate with, and it determines which features are achievable without client mods.

**Decision.** Target Minecraft 1.21.1 first, with the protocol layer written
multi-version from day one so later versions are additive rather than a rewrite.

**Reasoning.**
- It is the version of the existing VanillaPlusTerra pack, so there is a real 114-mod,
  1,055-structure workload to test against from day one rather than a synthetic one.
- It has the largest stable mod and plugin ecosystem of any recent version.
- **Decisively:** 1.20.5 moved items to data components and 1.21 made enchantments fully
  data-driven. Those two changes are what make the custom item builder and custom enchantment
  builder work on *unmodified* clients. On 1.20 both features would require a client mod, which
  contradicts the entire design premise.

**Consequences.**
- Dust launches against a version that is not the newest, which will be questioned. The answer is
  the ecosystem, and it is a good answer.
- The multi-version dimension must exist in the codec layer from the first commit. Retrofitting
  it later is a rewrite, and this is the single most common architectural regret in this space.
