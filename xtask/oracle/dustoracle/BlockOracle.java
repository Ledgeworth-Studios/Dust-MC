package dustoracle;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.PrintStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.InvocationHandler;
import java.lang.reflect.Method;
import java.lang.reflect.Proxy;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Locale;
import java.util.Map;
import java.util.function.Predicate;

/**
 * Prints, for every block state Minecraft has, the constants that exist only
 * as Java code: how much light entering the state costs, how much it emits,
 * whether it occludes, whether a player can stand inside it, which of the six
 * heightmaps count it, and the sound it makes when somebody puts it down.
 *
 * None of it is in any `--reports` output or any data pack, which is decision
 * record 0008 for the light values and the sound group, and 0010 for the
 * heightmap predicates. This asks the game.
 *
 * Nothing here names a Minecraft class: every identifier comes from the
 * properties file the extractor wrote from Mojang's published mappings, so a
 * version that renames the world changes that file and not this one.
 *
 * The output is tab-separated with a **named header**, and the names are load
 * bearing: `state_id`, `opacity`, `emission`, `occlude`, `replaceable`,
 * `full_collision`, `place_sound`, `sound_volume`, `sound_pitch`, one column per
 * heightmap under the serialization key Minecraft itself gives it, then
 * `STURDY_DOWN` through `STURDY_EAST`. A reader that matched columns by
 * position would silently change meaning the day a column was inserted; one
 * that reads the header can also say which columns a table it has been handed
 * does not have.
 *
 * `place_sound` is the sound event's **name** and not its registry id, for the
 * same reason the heightmap columns are keyed by serialization key: a name is
 * something the Rust side already knows independently, out of its own generated
 * `minecraft:sound_event` table, so the two meet on a string rather than on a
 * position one of them had to be told.
 */
public final class BlockOracle {

    public static void main(String[] args) throws Exception {
        if (args.length != 4) {
            System.err.println(
                "usage: BlockOracle <names.properties> <states.tsv> <items.tsv> "
                + "<blocks.tsv>");
            System.exit(2);
        }
        Names names = new Names(args[0]);

        // Minecraft's static initialisation. Block states do not exist until
        // this has run, and the registry below is empty rather than absent if
        // it has not — which would print a table of nothing and look like a
        // version with no blocks in it.
        runBootstrap(names);

        Object registry = names.field("block.class", "block.state_registry").get(null);
        Field lightEmission = names.field("blockstate.class", "blockstate.light_emission");
        Method getLightBlock = names.method(
            "blockstate.class",
            "blockstate.get_light_block",
            names.type("block_getter.class"),
            names.type("blockpos.class"));
        Method canOcclude = names.method("blockstate.class", "blockstate.can_occlude");
        // Whether the block yields anything at all to the wrong tool, and how
        // long it takes before a tool is considered. Both are per *state* in
        // the game — the field and the method are on `BlockStateBase` — so
        // both are read per state here rather than per block, which costs one
        // column each and asks nothing about whether vanilla happens to keep
        // them uniform across a block's states today.
        Method requiresCorrectTool = names.method(
            "blockstate.class", "blockstate.requires_correct_tool_for_drops");
        Field destroySpeed = names.field("blockstate.class", "blockstate.destroy_speed");
        Method canBeReplaced = names.method("blockstate.class", "blockstate.can_be_replaced");
        // Whether the *collision* shape is the whole cube, which is not any of
        // the other booleans here. A stair, a slab, a farmland block and a lump
        // of soul sand all block motion and all let a player stand somewhere
        // inside the cube they occupy; glass is not opaque and stops a player
        // dead. This is the only one of them a movement check can use.
        Method isCollisionShapeFullBlock = names.method(
            "blockstate.class",
            "blockstate.is_collision_shape_full_block",
            names.type("block_getter.class"),
            names.type("blockpos.class"));
        SoundGroups sounds = new SoundGroups(names);

        // The level Minecraft itself passes where there is no world, and the
        // origin. `getLightBlock` takes both and, for every vanilla block,
        // reads neither: opacity is a property of the state. A state that did
        // read them would be one this table cannot describe.
        Object emptyLevel = names.field("empty_block_getter.class", "empty_block_getter.instance").get(null);
        Object origin = names.field("blockpos.class", "blockpos.zero").get(null);

        List<Heightmap> heightmaps = heightmaps(names);

        // Whether the block falls when nothing holds it up, and whether the
        // state can stay where it is. Both are the same kind of thing as
        // everything above — Java, in no report and no data pack — and both
        // are what a world that reacts to being changed is made of.
        Method getBlock = names.method("blockstate.class", "blockstate.get_block");
        Class<?> fallingBlock = names.type("falling_block.class");
        Support support = new Support(names, emptyLevel, registry, getBlock);

        // How far a leaf is from the nearest log, which is what decides
        // whether a felled tree's canopy stays in the air.
        //
        // `getOptionalDistanceAt` is Minecraft's own whole relation: zero for
        // anything in `BlockTags.LOGS`, the state's own `distance` for a leaf,
        // and absent for everything else. **It answers only half of that here,
        // and the half it drops is silent.** This oracle runs Minecraft's
        // static initialisation and nothing else, and a block tag's contents
        // arrive from the data pack when a server loads one — so
        // `BlockTags.LOGS` is empty, every log falls through to the property
        // test, and every log is reported as having no answer. The count
        // printed as `log_states` is what says so: it is zero, and a rule that
        // read this column alone would put every leaf in the world at distance
        // seven and decay the tree it is still attached to. Dust takes the log
        // half from its own tag table, which is data and is extracted, and
        // this column carries the half that is Java.
        //
        // The number itself, where there is one, is a property of the state
        // and is already in the protocol table both sides share, so one flag
        // column carries the whole of what is left. The extractor checks that
        // claim below rather than assuming it.
        Method optionalDistanceAt = names.method(
            "leaves_block.class", "leaves_block.optional_distance_at",
            names.type("blockstate.concrete_class"));

        // Whether the state has a full square face on each of the six sides,
        // which is what a fence, a wall and a glass pane ask of the block
        // beside them before they grow an arm towards it. It is the block's
        // collision shape and so it is Java, in no report and no data pack —
        // the same argument decision record 0008 makes about opacity, and the
        // same route.
        //
        // **Six columns and not one.** A block can have a full face on one
        // side and not another, and the commonest such block is a stair: the
        // back of a bottom stair is a full square and its front is not, so a
        // fence joins a stair from behind and not from in front. One column
        // saying "is this a full cube" would answer no to both and would be
        // wrong in a place a player looks at all the time.
        Method isFaceSturdy = names.method(
            "blockstate.class",
            "blockstate.is_face_sturdy",
            names.type("block_getter.class"),
            names.type("blockpos.class"),
            names.type("direction.class"));
        String[] sideKeys = {
            "direction.down", "direction.up", "direction.north",
            "direction.south", "direction.west", "direction.east"
        };
        String[] sideNames = {
            "STURDY_DOWN", "STURDY_UP", "STURDY_NORTH",
            "STURDY_SOUTH", "STURDY_WEST", "STURDY_EAST"
        };
        Object[] sides = new Object[sideKeys.length];
        for (int i = 0; i < sideKeys.length; i++) {
            sides[i] = names.field("direction.class", sideKeys[i]).get(null);
        }

        Method getId = names.method("idmapper.class", "idmapper.get_id", Object.class);
        int written = 0;
        int sturdyFaces = 0;
        int falling = 0;
        int needsSupport = 0;
        int leaves = 0;
        int logs = 0;
        int distanceDisagreed = 0;
        try (BufferedWriter out = Files.newBufferedWriter(Path.of(args[1]))) {
            StringBuilder header = new StringBuilder(
                "# state_id\topacity\temission\tocclude\treplaceable"
                + "\tfull_collision\trequires_tool\tdestroy_speed"
                + "\tplace_sound\tsound_volume\tsound_pitch");
            for (Heightmap heightmap : heightmaps) {
                header.append('\t').append(heightmap.key);
            }
            for (String side : sideNames) {
                header.append('\t').append(side);
            }
            header.append("\tfalls\tLEAF_DISTANCE\tSURVIVES_ALONE");
            for (String side : Support.COLUMNS) {
                header.append('\t').append(side);
            }
            out.write(header.append('\n').toString());
            for (Object state : (Iterable<?>) registry) {
                int id = (int) getId.invoke(registry, state);
                int opacity = (int) getLightBlock.invoke(state, emptyLevel, origin);
                int emission = lightEmission.getInt(state);
                boolean occludes = (boolean) canOcclude.invoke(state);
                boolean replaceable = (boolean) canBeReplaced.invoke(state);
                boolean fullCollision = (boolean)
                    isCollisionShapeFullBlock.invoke(state, emptyLevel, origin);
                SoundGroup sound = sounds.of(state);
                StringBuilder row = new StringBuilder();
                row.append(id).append('\t')
                   .append(opacity).append('\t')
                   .append(emission).append('\t')
                   .append(occludes ? 1 : 0).append('\t')
                   .append(replaceable ? 1 : 0).append('\t')
                   .append(fullCollision ? 1 : 0).append('\t')
                   .append((boolean) requiresCorrectTool.invoke(state) ? 1 : 0).append('\t')
                   .append(destroySpeed.getFloat(state)).append('\t')
                   .append(sound.placeSound).append('\t')
                   .append(sound.volume).append('\t')
                   .append(sound.pitch);
                for (Heightmap heightmap : heightmaps) {
                    row.append('\t').append(heightmap.counts.test(state) ? 1 : 0);
                }
                for (Object side : sides) {
                    boolean sturdy = (boolean) isFaceSturdy.invoke(state, emptyLevel, origin, side);
                    row.append('\t').append(sturdy ? 1 : 0);
                    if (sturdy) {
                        sturdyFaces++;
                    }
                }
                boolean falls = fallingBlock.isInstance(getBlock.invoke(state));
                row.append('\t').append(falls ? 1 : 0);
                if (falls) {
                    falling++;
                }
                java.util.OptionalInt distance =
                    (java.util.OptionalInt) optionalDistanceAt.invoke(null, state);
                boolean known = distance.isPresent();
                boolean zero = known && distance.getAsInt() == 0;
                row.append('\t').append(known && !zero ? 1 : 0);
                if (zero) {
                    logs++;
                } else if (known) {
                    leaves++;
                    // The claim the two columns rest on: where Minecraft has a
                    // non-zero answer, that answer is the state's own
                    // `distance` property, which the Rust side already has out
                    // of the protocol table. If a version ever makes those two
                    // different this says so instead of Dust quietly
                    // restating a rule that has stopped being true.
                    if (distance.getAsInt() != leafDistanceProperty(state)) {
                        distanceDisagreed++;
                    }
                }
                boolean[] survives = support.of(state);
                for (boolean answer : survives) {
                    row.append('\t').append(answer ? 1 : 0);
                }
                if (!survives[0]) {
                    needsSupport++;
                }
                out.write(row.append('\n').toString());
                written++;
            }
        }
        System.out.println("states=" + written);
        System.out.println("heightmaps=" + heightmaps.size());
        System.out.println("sturdy_faces=" + sturdyFaces);
        System.out.println("falling_states=" + falling);
        System.out.println("needs_support=" + needsSupport);
        System.out.println("leaf_states=" + leaves);
        // Expected to be zero, and it is the evidence for the note above: a
        // bare static initialisation has loaded no data pack, so the log tag
        // `getOptionalDistanceAt` consults is empty. Printed rather than
        // asserted because a version that made it non-zero would be telling
        // this oracle something true and new.
        System.out.println("log_states=" + logs);
        System.out.println("leaf_distance_disagreed=" + distanceDisagreed);
        System.out.println("unaskable_probes=" + support.unaskable());
        System.out.println("unsupported_states=" + support.unsupported());
        System.out.println("sound_groups=" + sounds.seen());
        System.out.println("items=" + writeItems(names, Path.of(args[2])));
        System.out.println("blocks=" + writeBlocks(names, Path.of(args[3])));
    }

    /**
     * The `distance` a state's own properties say it is at.
     *
     * Read off `toString`, which is the one name on a Minecraft class that
     * obfuscation cannot take away, and used for one purpose: to check that
     * Minecraft's `getOptionalDistanceAt` and the state's own property agree,
     * so that the Rust side may read the number out of the protocol table it
     * already has instead of being handed a third column. `-1` where there is
     * no such property, which makes a disagreement rather than hiding one.
     */
    private static int leafDistanceProperty(Object state) {
        String text = state.toString();
        int at = text.indexOf("distance=");
        if (at < 0) {
            return -1;
        }
        int end = at + "distance=".length();
        int stop = end;
        while (stop < text.length() && Character.isDigit(text.charAt(stop))) {
            stop++;
        }
        if (stop == end) {
            return -1;
        }
        return Integer.parseInt(text.substring(end, stop));
    }

    /**
     * Whether a block state can stay where it is, and which neighbour it is
     * standing on.
     *
     * `canSurvive` is what breaks a torch off a wall that is mined, drops a
     * rail whose ground is dug out and pops a flower off dirt. It is Java, it
     * is per state, and it takes a `LevelReader` — which is the whole reason
     * it was out of reach until now. `Level` is an abstract class and cannot
     * be faked, which is the wall `tools/bot/placement.js` documents for
     * `getStateForPlacement`. **`LevelReader` is an interface**, and an
     * interface can be implemented by a `java.lang.reflect.Proxy` that answers
     * `getBlockState` out of a map of seven cells and hands everything else to
     * `EmptyBlockGetter`, which is the level Minecraft itself passes where
     * there is no world.
     *
     * # The two columns, and why one would have been useless
     *
     *   `SURVIVES_ALONE`  every neighbour is air. True for 20,110 of the
     *                     26,684 states, and it is the column a caller tests
     *                     first, because it costs one bit to answer "this
     *                     block does not care what happens around it".
     *   `SUPPORT_<side>`  **that one neighbour alone is enough.** Read as an
     *                     `or`: a state that names two sides survives on
     *                     either.
     *
     * A first version probed with stone and nothing else, and it said a
     * sapling has no support at all — because a sapling wants dirt and stone
     * is not dirt. A rule reading that column would have deleted every flower
     * and every sapling in the world the first time anything near one changed.
     * So the probe sweeps [`MATERIALS`] and stops at the first that the state
     * survives on, and what the column means is **which side is load bearing**
     * rather than what may be underneath it. That is the question a neighbour
     * update actually asks, because the thing that happens to a support in
     * play is that it is mined, and air holds nothing up.
     *
     * # What it does not answer, in counts rather than in adjectives
     *
     * A state that needs **two** neighbours at once — a sugar cane wants sand
     * under it *and* water beside that sand, which is a cell this
     * neighbourhood does not contain — comes out with `SURVIVES_ALONE` false
     * and no side named. Nothing breaks it, which is the safe direction: a
     * world that keeps a block it should have dropped is wrong in a way a
     * player calls a bug in one block, and a world that drops blocks it should
     * have kept is wrong in a way they call the server eating their build.
     * The count is printed as `unsupported_states`.
     */
    private static final class Support {
        /** The six per-face columns, in the order `Direction` declares. */
        static final String[] COLUMNS = {
            "SUPPORT_DOWN", "SUPPORT_UP", "SUPPORT_NORTH",
            "SUPPORT_SOUTH", "SUPPORT_WEST", "SUPPORT_EAST"
        };

        /** The six offsets, in the same order as the columns. */
        private static final int[][] OFFSETS = {
            {0, -1, 0}, {0, 1, 0}, {0, 0, -1}, {0, 0, 1}, {-1, 0, 0}, {1, 0, 0}
        };

        /**
         * What the probe puts next to a state, in the order it tries them.
         *
         * One per family of `mayPlaceOn` predicate rather than a list of
         * everything: stone is what a torch, a rail and a lever want, the
         * three soils are what the plants want, farmland is what the crops
         * want, and the last few are the odd ones that name a single block.
         * The sweep stops at the first material that the state survives on, so
         * the order is a cost and never an answer — a state that survives on
         * two of them names the same sides either way.
         */
        private static final String[] MATERIALS = {
            "minecraft:stone",
            "minecraft:dirt",
            "minecraft:grass_block",
            "minecraft:farmland",
            "minecraft:sand",
            "minecraft:soul_sand",
            "minecraft:soul_soil",
            "minecraft:end_stone",
            "minecraft:netherrack",
            "minecraft:moss_block",
            "minecraft:hay_block",
            "minecraft:honey_block",
            "minecraft:water",
            "minecraft:oak_log"
        };

        private final Method canSurvive;
        private final Object level;
        private final Object origin;
        private final Object[] sides = new Object[6];
        private final Map<Object, Object> cells = new HashMap<>();
        private final List<Object> materials = new ArrayList<>();
        /** Reused by [`withSelf`], so a sweep is not fifteen allocations. */
        private final List<Object> sweep = new ArrayList<>();
        private final Object air;
        private int unaskable;
        private int unsupported;

        Support(Names names, Object emptyLevel, Object states, Method getBlock) throws Exception {
            Class<?> blockPosClass = names.type("blockpos.class");
            Constructor<?> blockPos =
                names.constructor("blockpos.class", int.class, int.class, int.class);
            this.origin = blockPos.newInstance(0, 0, 0);
            for (int i = 0; i < OFFSETS.length; i++) {
                sides[i] = blockPos.newInstance(OFFSETS[i][0], OFFSETS[i][1], OFFSETS[i][2]);
            }
            this.canSurvive = names.method(
                "blockstate.class",
                "blockstate.can_survive",
                names.type("levelreader.class"),
                blockPosClass);

            // The states the probe puts in the world. Found by walking the
            // registry rather than by naming a class, for the reason the whole
            // oracle walks `BLOCK_STATE_REGISTRY`: the ids are the ones the
            // Rust side already has, and a name lookup would be a second place
            // for the two vocabularies to disagree. A material this version
            // does not have is skipped rather than fatal.
            Object blocks = names.field("builtin_registries.class", "builtin_registries.block")
                .get(null);
            Method getKey = names.method("registry.class", "registry.get_key", Object.class);
            Object foundAir = null;
            Object[] found = new Object[MATERIALS.length];
            for (Object state : (Iterable<?>) states) {
                String key = getKey.invoke(blocks, getBlock.invoke(state)).toString();
                if (foundAir == null && key.equals("minecraft:air")) {
                    foundAir = state;
                }
                for (int i = 0; i < MATERIALS.length; i++) {
                    if (found[i] == null && key.equals(MATERIALS[i])) {
                        found[i] = state;
                    }
                }
            }
            if (foundAir == null) {
                throw new IllegalStateException(
                    "the block state registry has no minecraft:air, so the support probe has "
                    + "nothing to build an empty neighbourhood out of");
            }
            this.air = foundAir;
            for (Object material : found) {
                if (material != null) {
                    materials.add(material);
                }
            }
            if (materials.isEmpty()) {
                throw new IllegalStateException(
                    "this version has none of the blocks the support probe stands things on");
            }

            Class<?> levelReader = names.type("levelreader.class");
            String getBlockState = getBlockStateName(names);
            InvocationHandler handler = (proxy, method, arguments) -> {
                if (method.getName().equals(getBlockState)
                    && method.getParameterCount() == 1
                    && method.getParameterTypes()[0] == blockPosClass) {
                    Object there = cells.get(arguments[0]);
                    return there != null ? there : air;
                }
                // Everything `BlockGetter` declares is answered by the empty
                // level, which is what vanilla passes in the same situation.
                // Everything `LevelReader` adds on top of it — biomes, light,
                // chunk lookups — is answered with a blank, and a `canSurvive`
                // that reached for one of those throws and is counted.
                if (method.getDeclaringClass().isAssignableFrom(emptyLevel.getClass())) {
                    return method.invoke(emptyLevel, arguments);
                }
                return blank(method.getReturnType());
            };
            this.level = Proxy.newProxyInstance(
                levelReader.getClassLoader(), new Class<?>[] {levelReader}, handler);
        }

        /**
         * The obfuscated name of `BlockGetter.getBlockState`.
         *
         * Read out of the same table everything else is, so this file still
         * names nothing of Minecraft's.
         */
        private static String getBlockStateName(Names names) throws Exception {
            return names.method(
                "block_getter.class",
                "block_getter.get_block_state",
                names.type("blockpos.class")).getName();
        }

        /** Seven answers for one state: alone, then one per side. */
        boolean[] of(Object state) {
            boolean[] out = new boolean[7];
            out[0] = ask(state, -1, null);
            if (out[0]) {
                return out;
            }
            // The state's own block is the last material tried, and it is the
            // one that answers the top half of a door, a stalk of sugar cane
            // and a bamboo shoot: what holds those up is more of themselves.
            // Written as a sweep ending in `state` rather than as a special
            // case because it is the same question — "what, on this side, is
            // enough" — and because the top half of a door being left floating
            // over a doorway that was mined is the most visible way this
            // column can be wrong.
            for (Object material : withSelf(state)) {
                boolean any = false;
                for (int i = 0; i < 6; i++) {
                    if (ask(state, i, material)) {
                        out[i + 1] = true;
                        any = true;
                    }
                }
                // The first material that holds this state up is the answer.
                // A later one names the same sides — a torch stands on stone
                // and on dirt and on both for the same reason — so carrying on
                // would cost thirteen more sweeps to learn nothing.
                if (any) {
                    return out;
                }
            }
            unsupported++;
            return out;
        }

        /** The material sweep for one state, ending in the state itself. */
        private List<Object> withSelf(Object state) {
            sweep.clear();
            sweep.addAll(materials);
            sweep.add(state);
            return sweep;
        }

        /** How many probes threw rather than answering. */
        int unaskable() {
            return unaskable;
        }

        /** How many states need something and named no side. */
        int unsupported() {
            return unsupported;
        }

        private boolean ask(Object state, int side, Object material) {
            cells.clear();
            // The state itself is in its own cell, because some rules read it:
            // the upper half of a door asks what is below it and finds the
            // lower half there, or does not.
            cells.put(origin, state);
            if (side >= 0) {
                cells.put(sides[side], material);
            }
            try {
                return (boolean) canSurvive.invoke(state, level, origin);
            } catch (Throwable failed) {
                unaskable++;
                return true;
            }
        }

        /** A value of the right shape for a method the proxy cannot answer. */
        private static Object blank(Class<?> type) {
            if (!type.isPrimitive()) {
                return null;
            }
            if (type == boolean.class) {
                return Boolean.FALSE;
            }
            if (type == int.class) {
                return 0;
            }
            if (type == long.class) {
                return 0L;
            }
            if (type == float.class) {
                return 0.0f;
            }
            if (type == double.class) {
                return 0.0d;
            }
            if (type == short.class) {
                return (short) 0;
            }
            if (type == byte.class) {
                return (byte) 0;
            }
            if (type == char.class) {
                return (char) 0;
            }
            return null;
        }
    }

    /** One heightmap: the name a chunk's NBT calls it, and what it counts. */
    private record Heightmap(String key, Predicate<Object> counts) {}

    /**
     * Every heightmap Minecraft has, in the order it declares them, each
     * carrying its own predicate.
     *
     * By enum constant and serialization key rather than by name or ordinal.
     * The keys are `WORLD_SURFACE_WG` and the rest — the strings a chunk's NBT
     * already uses, so the Rust side matches on something it independently
     * knows rather than on a position in this file.
     */
    @SuppressWarnings("unchecked")
    private static List<Heightmap> heightmaps(Names names) throws Exception {
        Class<?> types = names.type("heightmap_types.class");
        Method serializationKey = names.method("heightmap_types.class", "heightmap_types.serialization_key");
        Method isOpaque = names.method("heightmap_types.class", "heightmap_types.is_opaque");
        Object[] constants = types.getEnumConstants();
        if (constants == null || constants.length == 0) {
            throw new IllegalStateException(
                "the heightmap type resolved to something that is not an enum; check "
                + "`heightmap_types.class` against this version's mappings");
        }
        List<Heightmap> out = new ArrayList<>(constants.length);
        for (Object constant : constants) {
            String key = (String) serializationKey.invoke(constant);
            Predicate<Object> counts = (Predicate<Object>) isOpaque.invoke(constant);
            out.add(new Heightmap(key, counts));
        }
        return out;
    }

    /**
     * Detect the version, then run `Bootstrap.bootStrap()`, with the noise it
     * makes sent nowhere.
     *
     * The version has to come first: Minecraft's own `Main` does it, and
     * without it static initialisation dies on "Game version not set" from
     * inside a class the stack trace names only by an obfuscated letter.
     *
     * Minecraft logs to stdout during static initialisation, and this
     * program's stdout is a line the extractor parses.
     *
     * **Both streams are put back afterwards, and `System.err` is the one that
     * matters.** `Bootstrap.bootStrap()` ends by wrapping both of them into a
     * logger that has no appender configured here, so a stack trace thrown
     * after this point goes nowhere at all: the JVM prints
     * `Exception in thread "main"` and then nothing, which reads like a
     * crash with no cause rather than like a redirected stream.
     */
    private static void runBootstrap(Names names) throws Exception {
        PrintStream out = System.out;
        PrintStream err = System.err;
        System.setOut(new PrintStream(OutputStream.nullOutputStream()));
        try {
            names.method("sharedconstants.class", "sharedconstants.try_detect_version")
                .invoke(null);
            names.method("bootstrap.class", "bootstrap.boot").invoke(null);
        } finally {
            System.setOut(out);
            System.setErr(err);
        }
    }

    /**
     * Write, for every item, the block it puts down — or nothing, for the items
     * that put nothing down.
     *
     * `BlockItem` is the whole answer and the whole question: an item either is
     * one, in which case the block it holds is what a right-click places, or it
     * is not, in which case there is no block to name. Nothing here consults a
     * placement context, and the Rust side is explicit that what it gets is the
     * block and not the *state* — a stair knows which way it faces from where
     * the player stood, and that is a different problem in a different place.
     *
     * Four columns and three of them are names: the item's, the block's, and
     * the block's **wall** form where it has one. A number would be a position
     * in whichever Minecraft this ran against.
     *
     * A sixth column, `burn`, is how many ticks the item burns for in a
     * furnace, out of `AbstractFurnaceBlockEntity.getFuel()`. That map is Java
     * code — literals for coal and lava, and whole item tags expanded for the
     * wood — and no report and no data pack on 1.21.1 carries any of it. It is
     * here rather than in a table of its own because it is a fact about an
     * item, the file is already a row per item, and a second file would be a
     * second thing an operator can forget to copy.
     *
     * The wall form is `StandingAndWallBlockItem.wallBlock`, a second field on
     * a subclass of `BlockItem`, and it is the one thing a rule cannot get at:
     * a torch and a wall torch, a sign and a wall sign, are related only by the
     * item that holds both. Fifty-three items have one. `attaches` beside it is
     * that class's `attachmentDirection` — `down` for a sign, which stands on
     * the ground, and `up` for a hanging sign, which does not — because the
     * same clicked face gives those two different answers.
     */
    private static int writeItems(Names names, Path out) throws Exception {
        Object items = names.field("builtin_registries.class", "builtin_registries.item").get(null);
        Object blocks = names.field("builtin_registries.class", "builtin_registries.block").get(null);
        Method getId = names.method("registry.class", "registry.get_id", Object.class);
        Method getKey = names.method("registry.class", "registry.get_key", Object.class);
        Class<?> blockItem = names.type("blockitem.class");
        Field held = names.field("blockitem.class", "blockitem.block");
        Class<?> standingAndWall = names.type("standingandwall.class");
        Field wallBlock = names.field("standingandwall.class", "standingandwall.wall_block");
        Field attachment = names.field("standingandwall.class", "standingandwall.attachment");
        // The item tags first, or six of `getFuel`'s lines expand to nothing
        // and the column that comes out is forty-one fuels where the game has
        // hundreds. See `ItemTags` for why that failure is silent.
        int[] tags = ItemTags.bindInto(names, items);
        System.out.println("item_tags=" + tags[0] + " tagged_items=" + tags[1]);
        // Built once. `getFuel` rebuilds the whole map on every call — it
        // expands six item tags to do it — and calling it per item would be
        // eleven hundred rebuilds of the same table.
        Map<?, ?> fuel = (Map<?, ?>) names.method("furnace.class", "furnace.get_fuel").invoke(null);

        int written = 0;
        int walls = 0;
        int fuels = 0;
        try (BufferedWriter writer = Files.newBufferedWriter(out)) {
            writer.write("# item_id\titem\tplaces\ton_wall\tattaches\tburn\n");
            for (Object item : (Iterable<?>) items) {
                int id = (int) getId.invoke(items, item);
                Object name = getKey.invoke(items, item);
                // `-` and not an empty field: a trailing tab is invisible in a
                // diff and in an editor, and a row whose last column vanished
                // would read as one that was never written.
                String places = "-";
                String onWall = "-";
                String attaches = "-";
                if (blockItem.isInstance(item)) {
                    places = getKey.invoke(blocks, held.get(item)).toString();
                }
                if (standingAndWall.isInstance(item)) {
                    onWall = getKey.invoke(blocks, wallBlock.get(item)).toString();
                    attaches = String.valueOf(attachment.get(item)).toLowerCase(Locale.ROOT);
                    walls++;
                }
                Object burns = fuel.get(item);
                String burn = "-";
                if (burns != null) {
                    burn = String.valueOf(((Number) burns).intValue());
                    fuels++;
                }
                writer.write(
                    id + "\t" + name + "\t" + places + "\t" + onWall + "\t" + attaches
                    + "\t" + burn + "\n");
                written++;
            }
        }
        System.out.println("wall_items=" + walls);
        System.out.println("fuel_items=" + fuels);
        return written;
    }


    /**
     * Write, for every block, the loot table it draws from.
     *
     * `Block.getLootTable()` is a **code** constant. Every table itself is a
     * file in the data pack the operator already holds, and 982 of the 1,060
     * blocks on 1.21.1 draw from a table of their own name — so a server that
     * matched file names to block names would be right about 982 of them and
     * would drop nothing for the other 78. About sixty of those 78 are wall
     * forms: `oak_wall_sign` draws from `blocks/oak_sign.json`, and there is
     * no rule about names that gets there, because `coral_wall_fan` does not
     * follow the same rule that `wall_sign` does and `potted_cactus` follows
     * neither.
     *
     * Three columns, two of them names: the block's own registry id is here
     * because the Rust side is indexed by it, and the two names are here
     * because a name is a thing both sides know independently.
     */
    private static int writeBlocks(Names names, Path out) throws Exception {
        Object blocks = names.field("builtin_registries.class", "builtin_registries.block").get(null);
        Method getId = names.method("registry.class", "registry.get_id", Object.class);
        Method getKey = names.method("registry.class", "registry.get_key", Object.class);
        Method getLootTable = names.method("blockbehaviour.class", "blockbehaviour.get_loot_table");
        Method location = names.method("resourcekey.class", "resourcekey.location");

        int written = 0;
        int elsewhere = 0;
        try (BufferedWriter writer = Files.newBufferedWriter(out)) {
            writer.write("# block_id\tblock\tloot_table\n");
            for (Object block : (Iterable<?>) blocks) {
                int id = (int) getId.invoke(blocks, block);
                String name = getKey.invoke(blocks, block).toString();
                String table = location.invoke(getLootTable.invoke(block)).toString();
                // `minecraft:stone` draws from `minecraft:blocks/stone`; the
                // comparison is against that spelling and not against the bare
                // name, because the namespace is part of both.
                String own = own(name);
                if (!table.equals(own)) {
                    elsewhere++;
                }
                writer.write(id + "\t" + name + "\t" + table + "\n");
                written++;
            }
        }
        System.out.println("blocks_drawing_elsewhere=" + elsewhere);
        if (elsewhere == 0) {
            throw new IllegalStateException(
                "every block draws from a table of its own name, which no version of "
                + "Minecraft says; `blockbehaviour.get_loot_table` resolved to something "
                + "that answers the same shape for every block");
        }
        return written;
    }

    /** The table id a block of this name would draw from if it drew from its own. */
    private static String own(String block) {
        int colon = block.indexOf(':');
        return colon < 0
            ? "minecraft:blocks/" + block
            : block.substring(0, colon + 1) + "blocks/" + block.substring(colon + 1);
    }

    /** One block sound group: what placing it sounds like, and how loud. */
    private record SoundGroup(String placeSound, float volume, float pitch) {}

    /**
     * A block state's sound group, resolved once per distinct group.
     *
     * Minecraft has a few dozen `SoundType` instances and tens of thousands of
     * block states, and every state shares one of them by identity. The cache
     * is keyed on that identity, so the count it reports is the number of
     * *distinct* groups the game handed out — which is the check worth having
     * here for the same reason the per-heightmap counts are: a field that
     * resolved to the wrong member would answer the same thing for every state,
     * and one group covering twenty-eight thousand states says so in one line.
     */
    private static final class SoundGroups {
        private final Method getSoundType;
        private final Field volume;
        private final Field pitch;
        private final Field placeSound;
        private final Method getLocation;
        private final java.util.IdentityHashMap<Object, SoundGroup> cache =
            new java.util.IdentityHashMap<>();

        SoundGroups(Names names) throws Exception {
            this.getSoundType = names.method("blockstate.class", "blockstate.get_sound_type");
            this.volume = names.field("soundtype.class", "soundtype.volume");
            this.pitch = names.field("soundtype.class", "soundtype.pitch");
            this.placeSound = names.field("soundtype.class", "soundtype.place_sound");
            this.getLocation = names.method("soundevent.class", "soundevent.get_location");
        }

        SoundGroup of(Object state) throws Exception {
            Object type = getSoundType.invoke(state);
            SoundGroup known = cache.get(type);
            if (known != null) {
                return known;
            }
            Object event = placeSound.get(type);
            // `ResourceLocation.toString()` is `namespace:path`, which is the
            // spelling the Rust side's own registry table uses. Nothing here
            // reaches into the location's fields: its `toString` is the format,
            // and reassembling it from parts would be a second opinion about
            // a string Minecraft already renders.
            String name = getLocation.invoke(event).toString();
            SoundGroup group = new SoundGroup(
                name, volume.getFloat(type), pitch.getFloat(type));
            cache.put(type, group);
            return group;
        }

        int seen() {
            return cache.size();
        }
    }
}
