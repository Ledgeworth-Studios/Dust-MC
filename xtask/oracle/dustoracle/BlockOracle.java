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
import java.util.function.Predicate;

/**
 * Prints, for every block state Minecraft has, the constants that exist only
 * as Java code: how much light entering the state costs, how much it emits,
 * whether it occludes, and which of the six heightmaps count it.
 *
 * None of it is in any `--reports` output or any data pack, which is decision
 * record 0008 for the light values and 0010 for the heightmap predicates. This asks the game.
 *
 * Nothing here names a Minecraft class: every identifier comes from the
 * properties file the extractor wrote from Mojang's published mappings, so a
 * version that renames the world changes that file and not this one.
 *
 * The output is tab-separated with a **named header**, and the names are load
 * bearing: `state_id`, `opacity`, `emission`, `occlude`, then one column per
 * heightmap under the serialization key Minecraft itself gives it. A reader
 * that matched columns by position would silently change meaning the day a
 * column was inserted; one that reads the header can also say which columns a
 * table it has been handed does not have.
 */
public final class BlockOracle {

    public static void main(String[] args) throws Exception {
        if (args.length != 2) {
            System.err.println("usage: BlockOracle <names.properties> <output.tsv>");
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

        // The level Minecraft itself passes where there is no world, and the
        // origin. `getLightBlock` takes both and, for every vanilla block,
        // reads neither: opacity is a property of the state. A state that did
        // read them would be one this table cannot describe.
        Object emptyLevel = names.field("empty_block_getter.class", "empty_block_getter.instance").get(null);
        Object origin = names.field("blockpos.class", "blockpos.zero").get(null);

        List<Heightmap> heightmaps = heightmaps(names);

        Method getId = names.method("idmapper.class", "idmapper.get_id", Object.class);
        int written = 0;
        try (BufferedWriter out = Files.newBufferedWriter(Path.of(args[1]))) {
            StringBuilder header = new StringBuilder("# state_id\topacity\temission\tocclude");
            for (Heightmap heightmap : heightmaps) {
                header.append('\t').append(heightmap.key);
            }
            out.write(header.append('\n').toString());
            for (Object state : (Iterable<?>) registry) {
                int id = (int) getId.invoke(registry, state);
                int opacity = (int) getLightBlock.invoke(state, emptyLevel, origin);
                int emission = lightEmission.getInt(state);
                boolean occludes = (boolean) canOcclude.invoke(state);
                StringBuilder row = new StringBuilder();
                row.append(id).append('\t')
                   .append(opacity).append('\t')
                   .append(emission).append('\t')
                   .append(occludes ? 1 : 0);
                for (Heightmap heightmap : heightmaps) {
                    row.append('\t').append(heightmap.counts.test(state) ? 1 : 0);
                }
                out.write(row.append('\n').toString());
                written++;
            }
        }
        System.out.println("states=" + written);
        System.out.println("heightmaps=" + heightmaps.size());
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
     */
    private static void runBootstrap(Names names) throws Exception {
        PrintStream out = System.out;
        System.setOut(new PrintStream(OutputStream.nullOutputStream()));
        try {
            names.method("sharedconstants.class", "sharedconstants.try_detect_version")
                .invoke(null);
            names.method("bootstrap.class", "bootstrap.boot").invoke(null);
        } finally {
            System.setOut(out);
        }
    }
}
