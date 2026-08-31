package dustoracle;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.PrintStream;
import java.io.OutputStream;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Iterator;

/**
 * Prints, for every block state Minecraft has, the two light constants that
 * exist only as Java code: how much light entering the state costs, and how
 * much it emits.
 *
 * Neither is in any `--reports` output or any data pack, which is the whole of
 * decision record 0008. This asks the game.
 *
 * Nothing here names a Minecraft class: every identifier comes from the
 * properties file the extractor wrote from Mojang's published mappings, so a
 * version that renames the world changes that file and not this one.
 */
public final class LightOracle {

    public static void main(String[] args) throws Exception {
        if (args.length != 2) {
            System.err.println("usage: LightOracle <names.properties> <output.tsv>");
            System.exit(2);
        }
        Names names = new Names(args[0]);

        // Minecraft's static initialisation. Block states do not exist until
        // this has run, and the registry below is empty rather than absent if
        // it has not — which would print a table of nothing and look like a
        // version with no blocks in it.
        runBootstrap(names);

        Object registry = names.field("block.class", "block.state_registry").get(null);
        Class<?> blockState = names.type("blockstate.class");
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
        // read them would be one this table cannot describe, and the check
        // below is what would say so.
        Object emptyLevel = names.field("empty_block_getter.class", "empty_block_getter.instance").get(null);
        Object origin = names.field("blockpos.class", "blockpos.zero").get(null);

        Method getId = names.method("idmapper.class", "idmapper.get_id", Object.class);
        int written = 0;
        try (BufferedWriter out = Files.newBufferedWriter(Path.of(args[1]))) {
            out.write("# state_id\topacity\temission\tocclude\n");
            for (Object state : (Iterable<?>) registry) {
                int id = (int) getId.invoke(registry, state);
                int opacity = (int) getLightBlock.invoke(state, emptyLevel, origin);
                int emission = lightEmission.getInt(state);
                boolean occludes = (boolean) canOcclude.invoke(state);
                out.write(id + "\t" + opacity + "\t" + emission + "\t" + (occludes ? 1 : 0) + "\n");
                written++;
            }
        }
        System.out.println("states=" + written);
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
