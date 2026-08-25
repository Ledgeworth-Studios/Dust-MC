<!-- Transcribed from the planning documents at Phase 0.1. The plan is the
     working copy; this file is the record. Where they differ, this one is
     what the project decided. -->

# D1 — Base: written from scratch

**Status:** Accepted 2026-08-21. Re-affirmed the same day after D2 reopened the option, on
technical grounds recorded below.

**Context.** Several Rust Minecraft servers already exist. Building on one would inherit years of
vanilla-parity work. Building from nothing means owning every line and every architectural
choice. When D2 settled on GPL-3.0, Pumpkin became legally available as a base and the question
was deliberately re-opened.

**Options considered.**

| Option | License | Maturity (2026-08-21) | What it would have given |
| --- | --- | --- | --- |
| **From scratch** ✅ | free choice | — | Nothing inherited. Full control, full cost. |
| Pumpkin | GPL-3.0 | 10,839 stars, commits daily | Vanilla parity, chunk generation, and PatchBukkit's working JVM plugin bridge |
| FerrumC | MIT | 2,382 stars, active | Networking, NBT/Anvil, an ECS entity system. No plugin support. |
| Valence | MIT | 3,265 stars, last push 2026-06-15 | A framework for custom servers, not a vanilla-parity server |
| SteelMC | AGPL-3.0 | 465 stars, pre-alpha | Little, and the most restrictive license of the set |

**Decision.** Dust is written from scratch. No existing server is forked, vendored or derived
from as a base.

**Consequences.**
- Three to four years to a usable release for one developer, against roughly twelve to eighteen
  months on a Pumpkin base. Stage B is the bulk of it.
- Full architectural control. In particular, the threading model is regionised from the first
  line rather than retrofitted onto something written single-threaded.
- No upstream to track, rebase against, or be surprised by.
- **"From scratch" is a default, not a vow.** D2 settled on GPL-3.0 precisely so that existing
  implementations remain available. Where a GPL-3.0 implementation solves a problem well, it may
  be incorporated with attribution rather than retyped from memory out of pride. What D1 decides
  is architectural independence, not a refusal to ever use anyone else's line of code.
**Why the re-examination confirmed it: the inheritance is not clean.** The base worth taking
would have been Pumpkin, and Pumpkin's weakest area is precisely the area Dust cannot afford to
be weak in. Its open issue tracker carries a standing set of chunk generation defects — verified
2026-08-21:

| Issue | |
| --- | --- |
| #2362 | Incorrect terrain generation in the Jungle |
| #1949 | Incorrect ore and stone generation |
| #2027 | Random blocks generating mid-grass, underfilled trees |
| #2151 | Visible clear-cut biome borders at chunk boundaries in badlands |
| #3005 | Stronghold ring placement samples biomes from the triggering chunk |
| #2626 | Chunk writer drops block entities, structures and post-processing when saving |
| #2780, #2885 | Deep dark generation; glow lichen generating as full blocks |

Inheriting a generator means inheriting its defects *and* its architecture, and worldgen defects
are the expensive kind — they are latent in every chunk already written to disk, so fixing one
does not repair the world it already produced. Terrain generation is also the feature this
project is most opinionated about, per Phase 11.

Note what that list validates: every one of those is a *parity* defect, the class that unit tests
pass straight over and only seed-for-seed differential testing against a real vanilla server
catches. That is why the Phase 6 exit criterion is block-identical output across ten seeds rather
than "the terrain looks right", and it is the single most load-bearing test in the plan.
