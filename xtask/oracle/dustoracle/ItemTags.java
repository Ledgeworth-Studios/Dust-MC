package dustoracle;

import com.google.gson.JsonArray;
import com.google.gson.JsonElement;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;

import java.io.InputStreamReader;
import java.io.Reader;
import java.lang.reflect.Method;
import java.net.URI;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.jar.JarEntry;
import java.util.jar.JarFile;

/**
 * Binds Minecraft's item tags to the item registry, out of the vanilla data
 * pack that is inside the operator's own server jar.
 *
 * # Why this exists at all
 *
 * `AbstractFurnaceBlockEntity.getFuel()` is the only source for how long an
 * item burns, and six of its lines are not items but *tags*: every plank
 * burns for 300 ticks, every log that burns for 300, all wool for 100. It
 * expands them through `Registry.getTagOrEmpty`, and after `Bootstrap` has run
 * nothing has bound a single tag — so those six lines expand to nothing and
 * the map that comes back has forty-one entries where the game has hundreds.
 *
 * **Nothing says so.** An unbound tag is empty, not absent; the call succeeds,
 * the map is well-formed, and every fuel a player actually burns is quietly
 * not in it. That is the shape of defect this project keeps meeting: a
 * stand-in whose range does not reach the case, answering confidently.
 *
 * So the tags are bound first, from `data/&lt;namespace&gt;/tags/item/*.json`
 * inside the jar the oracle is already reflecting into. Same jar, same files,
 * the same ones Minecraft's own tag loader reads.
 *
 * # Resolving is recursive, and that is not an optimisation
 *
 * A tag's `values` may name another tag with a leading `#`, and vanilla's
 * fuel tags do: `#logs_that_burn` is six other tags and nothing else, and
 * binding it to its direct members would bind it to nothing at all — the same
 * silent emptiness one level down. So references are followed, with a visited
 * set so a data pack that made a cycle stops rather than recurses for ever.
 *
 * A member may be a bare name or an object with `id` and `required`. An
 * optional member naming an item this version does not have is skipped, which
 * is what the word means; a required one that is missing is skipped too and
 * counted, because the oracle's job is to report what the jar says and not to
 * referee the operator's data pack.
 */
final class ItemTags {

    private ItemTags() {
    }

    /**
     * Read every item tag out of the jar and bind it to the item registry.
     *
     * @return how many tags were bound, and how many items they name between
     *     them, as a two-element array — printed by the caller so a run that
     *     bound nothing is visible rather than merely producing a short table.
     */
    static int[] bindInto(Names names, Object items) throws Exception {
        Map<String, List<String>> raw = readFromJar(items.getClass());
        if (raw.isEmpty()) {
            return new int[] {0, 0};
        }

        Method parse = names.method(
            "resourcelocation.class", "resourcelocation.parse", String.class);
        Method create = names.method(
            "tagkey.class",
            "tagkey.create",
            names.type("resourcekey.class"),
            names.type("resourcelocation.class"));
        Method registryKey = names.method("registry.class", "registry.key");
        Method getHolder = names.method(
            "registry.class", "registry.get_holder", names.type("resourcelocation.class"));
        Method bindTags = names.method("registry.class", "registry.bind_tags", Map.class);

        Object itemRegistryKey = registryKey.invoke(items);
        Map<Object, List<Object>> bound = new LinkedHashMap<>();
        int named = 0;
        for (Map.Entry<String, List<String>> entry : raw.entrySet()) {
            Set<String> members = new HashSet<>();
            resolve(raw, entry.getKey(), new HashSet<>(), members);
            List<Object> holders = new ArrayList<>(members.size());
            for (String member : members) {
                Object location = parse.invoke(null, member);
                Object holder = ((java.util.Optional<?>) getHolder.invoke(items, location))
                    .orElse(null);
                if (holder != null) {
                    holders.add(holder);
                }
            }
            named += holders.size();
            bound.put(create.invoke(null, itemRegistryKey, parse.invoke(null, entry.getKey())),
                holders);
        }
        bindTags.invoke(items, bound);
        return new int[] {bound.size(), named};
    }

    /**
     * Every `data/&lt;namespace&gt;/tags/item/**.json` in the jar the item
     * registry's own class was loaded from, as tag name to raw member list.
     *
     * The jar is found through the class rather than passed in, so this cannot
     * be pointed at a different jar than the one the rest of the oracle is
     * reflecting into. Two jars would be two versions and a table mixing them
     * would be wrong in a way nothing here could detect.
     */
    private static Map<String, List<String>> readFromJar(Class<?> anchor) throws Exception {
        Map<String, List<String>> raw = new HashMap<>();
        URI source = anchor.getProtectionDomain().getCodeSource().getLocation().toURI();
        try (JarFile jar = new JarFile(Path.of(source).toFile())) {
            Enumeration<JarEntry> entries = jar.entries();
            while (entries.hasMoreElements()) {
                JarEntry entry = entries.nextElement();
                String path = entry.getName();
                if (!path.startsWith("data/") || !path.endsWith(".json")) {
                    continue;
                }
                String[] parts = path.split("/", 3);
                if (parts.length != 3 || !parts[2].startsWith("tags/item/")) {
                    continue;
                }
                String name = parts[1] + ":"
                    + parts[2].substring("tags/item/".length(), parts[2].length() - ".json".length());
                try (Reader reader = new InputStreamReader(
                        jar.getInputStream(entry), StandardCharsets.UTF_8)) {
                    raw.put(name, members(JsonParser.parseReader(reader)));
                }
            }
        }
        return raw;
    }

    /** The `values` of one tag file, as written — `#` prefixes intact. */
    private static List<String> members(JsonElement file) {
        List<String> out = new ArrayList<>();
        if (!file.isJsonObject()) {
            return out;
        }
        JsonElement values = file.getAsJsonObject().get("values");
        if (values == null || !values.isJsonArray()) {
            return out;
        }
        for (JsonElement value : (JsonArray) values) {
            if (value.isJsonPrimitive()) {
                out.add(value.getAsString());
            } else if (value.isJsonObject()) {
                JsonObject object = value.getAsJsonObject();
                if (object.has("id")) {
                    out.add(object.get("id").getAsString());
                }
            }
        }
        return out;
    }

    /**
     * Everything `tag` names, following `#` references.
     *
     * `seen` is not an optimisation: a data pack whose tags refer to each
     * other in a circle would otherwise recurse until the stack ran out, and
     * the operator would get a crash where they should get a table.
     */
    private static void resolve(
            Map<String, List<String>> raw, String tag, Set<String> seen, Set<String> out) {
        if (!seen.add(tag)) {
            return;
        }
        List<String> values = raw.get(namespaced(tag));
        if (values == null) {
            return;
        }
        for (String value : values) {
            if (value.startsWith("#")) {
                resolve(raw, namespaced(value.substring(1)), seen, out);
            } else {
                out.add(namespaced(value));
            }
        }
    }

    /** A bare name is `minecraft:`, which is what every loader here assumes. */
    private static String namespaced(String name) {
        return name.indexOf(':') < 0 ? "minecraft:" + name : name;
    }
}
