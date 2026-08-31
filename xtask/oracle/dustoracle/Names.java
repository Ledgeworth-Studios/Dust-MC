package dustoracle;

import java.io.FileInputStream;
import java.io.IOException;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.Properties;

/**
 * Looks up Minecraft's classes and members by the semantic keys this harness
 * uses, from a properties file the extractor wrote.
 *
 * The server jar is obfuscated, so nothing here can name what it calls. The
 * extractor parses Mojang's published mappings and hands over a key-to-name
 * table; this file therefore contains no Minecraft identifier at all, and no
 * part of it needs rewriting when a new version renames every class.
 */
final class Names {
    private final Properties table = new Properties();

    Names(String path) throws IOException {
        try (FileInputStream in = new FileInputStream(path)) {
            table.load(in);
        }
    }

    private String get(String key) {
        String value = table.getProperty(key);
        if (value == null) {
            throw new IllegalStateException(
                "the extractor did not supply a name for `" + key + "`");
        }
        return value;
    }

    Class<?> type(String key) throws ClassNotFoundException {
        return Class.forName(get(key));
    }

    Method method(String owner, String key, Class<?>... parameters) throws Exception {
        Method method = type(owner).getDeclaredMethod(get(key), parameters);
        method.setAccessible(true);
        return method;
    }

    Constructor<?> constructor(String owner, Class<?>... parameters) throws Exception {
        Constructor<?> constructor = type(owner).getDeclaredConstructor(parameters);
        constructor.setAccessible(true);
        return constructor;
    }

    Field field(String owner, String key) throws Exception {
        Field field = type(owner).getDeclaredField(get(key));
        field.setAccessible(true);
        return field;
    }
}
