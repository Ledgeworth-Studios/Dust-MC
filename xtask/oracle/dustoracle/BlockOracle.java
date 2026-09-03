package dustoracle;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.PrintStream;
import java.io.OutputStream;
import java.io.UncheckedIOException;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
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
                out.write(row.append('\n').toString());
                written++;
            }
        }
        System.out.println("states=" + written);
        System.out.println("heightmaps=" + heightmaps.size());
        System.out.println("sturdy_faces=" + sturdyFaces);
        System.out.println("sound_groups=" + sounds.seen());
        System.out.println("items=" + writeItems(names, Path.of(args[2])));
        System.out.println("blocks=" + writeBlocks(names, Path.of(args[3])));
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

        int written = 0;
        int walls = 0;
        try (BufferedWriter writer = Files.newBufferedWriter(out)) {
            writer.write("# item_id\titem\tplaces\ton_wall\tattaches\n");
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
                writer.write(
                    id + "\t" + name + "\t" + places + "\t" + onWall + "\t" + attaches + "\n");
                written++;
            }
        }
        System.out.println("wall_items=" + walls);
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
