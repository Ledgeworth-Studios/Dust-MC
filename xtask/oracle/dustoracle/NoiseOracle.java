package dustoracle;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;

/**
 * Runs Minecraft's own random sources and noise functions and prints what they
 * produce.
 *
 * This is the external oracle. Every other test in the worldgen work compares
 * Dust against Dust, which for a noise function is worth nothing: a Perlin
 * implementation agrees with itself under any consistent mistake. The only
 * question that matters is whether it produces the numbers Minecraft produces,
 * and this is the only thing on the machine that can answer it.
 *
 * Output is one row per value: `case`, `kind`, `value`. Doubles and floats are
 * printed as their raw IEEE-754 bit patterns, because a decimal rendering is a
 * lossy comparison and the whole point here is that the last bit matches.
 *
 * Nothing Mojang ships is copied out. What leaves this program is a list of
 * numbers computed on the machine that ran it.
 */
public final class NoiseOracle {
    private static final long[] SEEDS = {0L, 1L, 42L, -1L, 25214903917L, 1234567890123L};
    private static final int[] BOUNDS = {1, 2, 17, 256, 1000, 1073741824};
    private static final int[][] POSITIONS = {
        {0, 0, 0}, {1, 2, 3}, {-1, -1, -1}, {16, 64, -32},
        {1875, -17, 90210}, {-100000, 319, 100000},
    };
    private static final double[][] SAMPLES = {
        {0.0, 0.0, 0.0}, {1.5, 2.5, 3.5}, {-0.25, 60.0, 12.75},
        {1000.5, -47.0, -2000.25}, {0.001, 0.002, 0.003},
    };
    private static final String[] HASHED = {
        "octave_0", "octave_-7", "minecraft:temperature", "", "aquifer_barrier",
    };

    private final Names names;
    private final List<String> rows = new ArrayList<>();

    private NoiseOracle(Names names) {
        this.names = names;
    }

    public static void main(String[] argv) throws Exception {
        if (argv.length != 1) {
            System.err.println("usage: NoiseOracle <names.properties>");
            System.exit(2);
        }
        NoiseOracle oracle = new NoiseOracle(new Names(argv[0]));
        oracle.run();
        StringBuilder out = new StringBuilder();
        for (String row : oracle.rows) {
            out.append(row).append('\n');
        }
        System.out.print(out);
    }

    private void i64(String name, long value) {
        rows.add(name + "\ti64\t" + value);
    }

    private void i32(String name, int value) {
        rows.add(name + "\ti32\t" + value);
    }

    private void f64(String name, double value) {
        rows.add(name + "\tf64\t" + Double.doubleToRawLongBits(value));
    }

    private void f32(String name, float value) {
        rows.add(name + "\tf32\t" + Float.floatToRawIntBits(value));
    }

    private void run() throws Exception {
        randomSupport();
        xoroshiro128();
        randomSources();
        positionalFactories();
        improvedNoise();
        perlinNoise();
        normalNoise();
        blendedNoise();
    }

    // ---- RandomSupport: the seed mixing everything else is built on --------

    private void randomSupport() throws Exception {
        Method mix = names.method("RandomSupport", "RandomSupport#mixStafford13", long.class);
        Method upgrade = names.method("RandomSupport", "RandomSupport#upgradeSeedTo128bit", long.class);
        Method hashOf = names.method("RandomSupport", "RandomSupport#seedFromHashOf", String.class);
        Method lo = names.method("Seed128bit", "Seed128bit#seedLo");
        Method hi = names.method("Seed128bit", "Seed128bit#seedHi");
        Method getSeed = names.method("Mth", "Mth#getSeed", int.class, int.class, int.class);

        for (long seed : SEEDS) {
            i64("mix_stafford_13/" + seed, (Long) mix.invoke(null, seed));
            Object upgraded = upgrade.invoke(null, seed);
            i64("upgrade_seed_to_128/" + seed + "/lo", (Long) lo.invoke(upgraded));
            i64("upgrade_seed_to_128/" + seed + "/hi", (Long) hi.invoke(upgraded));
        }
        for (String text : HASHED) {
            Object seed = hashOf.invoke(null, text);
            i64("seed_from_hash_of/" + label(text) + "/lo", (Long) lo.invoke(seed));
            i64("seed_from_hash_of/" + label(text) + "/hi", (Long) hi.invoke(seed));
        }
        for (int[] p : POSITIONS) {
            i64("mth_get_seed/" + at(p), (Long) getSeed.invoke(null, p[0], p[1], p[2]));
        }
    }

    private void xoroshiro128() throws Exception {
        Constructor<?> make = names.constructor("Xoroshiro128PlusPlus", long.class, long.class);
        Method nextLong = names.method("Xoroshiro128PlusPlus", "Xoroshiro128PlusPlus#nextLong");
        long[][] pairs = {{0L, 0L}, {1L, 2L}, {-1L, 1L}, {0x9E3779B97F4A7C15L, 0x6A09E667F3BCC909L}};
        for (long[] pair : pairs) {
            Object generator = make.newInstance(pair[0], pair[1]);
            for (int i = 0; i < 8; i++) {
                i64("xoroshiro128/" + pair[0] + "," + pair[1] + "/" + i, (Long) nextLong.invoke(generator));
            }
        }
    }

    // ---- The two RandomSource implementations -----------------------------

    private void randomSources() throws Exception {
        drawFrom("legacy", names.constructor("LegacyRandomSource", long.class));
        drawFrom("xoroshiro", names.constructor("XoroshiroRandomSource", long.class));
    }

    /**
     * The same draw sequence from both sources.
     *
     * A separate instance per method on purpose: sharing one would make each
     * row depend on every row before it, so a single wrong draw would fail
     * every row after it and say nothing about which one was wrong.
     */
    private void drawFrom(String kind, Constructor<?> make) throws Exception {
        Method nextInt = names.method("RandomSource", "RandomSource#nextInt");
        Method nextIntBound = names.method("RandomSource", "RandomSource#nextIntBound", int.class);
        Method nextLong = names.method("RandomSource", "RandomSource#nextLong");
        Method nextBoolean = names.method("RandomSource", "RandomSource#nextBoolean");
        Method nextFloat = names.method("RandomSource", "RandomSource#nextFloat");
        Method nextDouble = names.method("RandomSource", "RandomSource#nextDouble");
        Method nextGaussian = names.method("RandomSource", "RandomSource#nextGaussian");
        Method consumeCount = names.method("RandomSource", "RandomSource#consumeCount", int.class);
        Method fork = names.method("RandomSource", "RandomSource#fork");

        for (long seed : SEEDS) {
            String base = kind + "/" + seed;
            Object r = make.newInstance(seed);
            for (int i = 0; i < 8; i++) {
                i32(base + "/next_int/" + i, (Integer) nextInt.invoke(r));
            }
            r = make.newInstance(seed);
            for (int i = 0; i < 8; i++) {
                i64(base + "/next_long/" + i, (Long) nextLong.invoke(r));
            }
            r = make.newInstance(seed);
            for (int i = 0; i < 8; i++) {
                f64(base + "/next_double/" + i, (Double) nextDouble.invoke(r));
            }
            r = make.newInstance(seed);
            for (int i = 0; i < 8; i++) {
                f32(base + "/next_float/" + i, (Float) nextFloat.invoke(r));
            }
            r = make.newInstance(seed);
            for (int i = 0; i < 8; i++) {
                i32(base + "/next_boolean/" + i, ((Boolean) nextBoolean.invoke(r)) ? 1 : 0);
            }
            // Gaussian draws in pairs and caches the second, so eight of them
            // exercise both the fresh path and the cached one.
            r = make.newInstance(seed);
            for (int i = 0; i < 8; i++) {
                f64(base + "/next_gaussian/" + i, (Double) nextGaussian.invoke(r));
            }
            for (int bound : BOUNDS) {
                r = make.newInstance(seed);
                for (int i = 0; i < 6; i++) {
                    i32(base + "/next_int_bound/" + bound + "/" + i,
                        (Integer) nextIntBound.invoke(r, bound));
                }
            }
            // consumeCount then a draw: this is what PerlinNoise does when it
            // skips an octave, and getting the skip length wrong shifts every
            // octave after it.
            for (int count : new int[] {1, 2, 262}) {
                r = make.newInstance(seed);
                consumeCount.invoke(r, count);
                i64(base + "/consume_count/" + count, (Long) nextLong.invoke(r));
            }
            r = make.newInstance(seed);
            Object forked = fork.invoke(r);
            i64(base + "/fork/first", (Long) nextLong.invoke(forked));
            i64(base + "/fork/parent_next", (Long) nextLong.invoke(r));
        }
    }

    // ---- Positional seeding: where a parity bug hides for months -----------

    private void positionalFactories() throws Exception {
        Method forkPositional = names.method("RandomSource", "RandomSource#forkPositional");
        Method at = names.method("PositionalRandomFactory", "PositionalRandomFactory#at",
            int.class, int.class, int.class);
        Method fromHashOf = names.method("PositionalRandomFactory", "PositionalRandomFactory#fromHashOf",
            String.class);
        Method fromSeed = names.method("PositionalRandomFactory", "PositionalRandomFactory#fromSeed",
            long.class);
        Method nextLong = names.method("RandomSource", "RandomSource#nextLong");

        Constructor<?>[] makers = {
            names.constructor("LegacyRandomSource", long.class),
            names.constructor("XoroshiroRandomSource", long.class),
        };
        String[] kinds = {"legacy", "xoroshiro"};

        for (int k = 0; k < makers.length; k++) {
            for (long seed : SEEDS) {
                Object factory = forkPositional.invoke(makers[k].newInstance(seed));
                String base = kinds[k] + "_positional/" + seed;
                for (int[] p : POSITIONS) {
                    Object r = at.invoke(factory, p[0], p[1], p[2]);
                    i64(base + "/at/" + at(p) + "/0", (Long) nextLong.invoke(r));
                    i64(base + "/at/" + at(p) + "/1", (Long) nextLong.invoke(r));
                }
                for (String text : HASHED) {
                    Object r = fromHashOf.invoke(factory, text);
                    i64(base + "/from_hash_of/" + label(text), (Long) nextLong.invoke(r));
                }
                for (long inner : new long[] {0L, 7L, -3L}) {
                    Object r = fromSeed.invoke(factory, inner);
                    i64(base + "/from_seed/" + inner, (Long) nextLong.invoke(r));
                }
            }
        }
    }

    // ---- The noise functions ----------------------------------------------

    private void improvedNoise() throws Exception {
        Constructor<?> make = names.constructor("ImprovedNoise", names.type("RandomSource"));
        Method noise3 = names.method("ImprovedNoise", "ImprovedNoise#noise3",
            double.class, double.class, double.class);
        Method noise5 = names.method("ImprovedNoise", "ImprovedNoise#noise5",
            double.class, double.class, double.class, double.class, double.class);
        Field xo = names.field("ImprovedNoise", "ImprovedNoise#xo");
        Field yo = names.field("ImprovedNoise", "ImprovedNoise#yo");
        Field zo = names.field("ImprovedNoise", "ImprovedNoise#zo");
        Field p = names.field("ImprovedNoise", "ImprovedNoise#p");

        Constructor<?>[] makers = {
            names.constructor("LegacyRandomSource", long.class),
            names.constructor("XoroshiroRandomSource", long.class),
        };
        String[] kinds = {"legacy", "xoroshiro"};

        for (int k = 0; k < makers.length; k++) {
            for (long seed : new long[] {0L, 42L, 1234567890123L}) {
                Object noise = make.newInstance(makers[k].newInstance(seed));
                String base = "improved_noise/" + kinds[k] + "/" + seed;
                f64(base + "/xo", (Double) xo.get(noise));
                f64(base + "/yo", (Double) yo.get(noise));
                f64(base + "/zo", (Double) zo.get(noise));
                // The permutation itself, as a checksum over all 256 entries.
                // A shuffle that is off by one draw produces a different table
                // and the same-looking noise, which is exactly the failure a
                // sampled comparison misses.
                byte[] permutation = (byte[]) p.get(noise);
                long sum = 0;
                for (int i = 0; i < permutation.length; i++) {
                    sum = sum * 1099511628211L + (permutation[i] & 0xFF);
                }
                i64(base + "/permutation_fnv", sum);
                i32(base + "/permutation_len", permutation.length);
                for (double[] s : SAMPLES) {
                    f64(base + "/noise/" + at(s), (Double) noise3.invoke(noise, s[0], s[1], s[2]));
                }
                // The five-argument form, whose y clamping is a distinct path
                // that the three-argument form never reaches.
                for (double[] s : SAMPLES) {
                    f64(base + "/noise_yclamp/" + at(s),
                        (Double) noise5.invoke(noise, s[0], s[1], s[2], 2.0, 1.0));
                }
            }
        }
    }

    private static final double[][] AMPLITUDE_SETS = {
        {1.0},
        {1.0, 1.0},
        {1.0, 1.0, 1.0, 1.0},
        {1.0, 0.0, 1.0},
        {0.0, 1.0, 2.0, 0.5},
        {1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0},
    };
    private static final int[] FIRST_OCTAVES = {0, -1, -3, -7, -9, 2};

    private void perlinNoise() throws Exception {
        Class<?> doubleList = Class.forName("it.unimi.dsi.fastutil.doubles.DoubleList");
        Method wrap = Class.forName("it.unimi.dsi.fastutil.doubles.DoubleArrayList")
            .getMethod("wrap", double[].class);
        Method create = names.method("PerlinNoise", "PerlinNoise#create",
            names.type("RandomSource"), int.class, doubleList);
        Method getValue = names.method("PerlinNoise", "PerlinNoise#getValue",
            double.class, double.class, double.class);
        Method maxValue = names.method("PerlinNoise", "PerlinNoise#maxValue");
        Constructor<?> xoro = names.constructor("XoroshiroRandomSource", long.class);

        for (int firstOctave : FIRST_OCTAVES) {
            for (int a = 0; a < AMPLITUDE_SETS.length; a++) {
                Object list = wrap.invoke(null, (Object) AMPLITUDE_SETS[a].clone());
                Object noise = create.invoke(null, xoro.newInstance(42L), firstOctave, list);
                String base = "perlin/" + firstOctave + "/" + a;
                f64(base + "/max_value", (Double) maxValue.invoke(noise));
                for (double[] s : SAMPLES) {
                    f64(base + "/value/" + at(s), (Double) getValue.invoke(noise, s[0], s[1], s[2]));
                }
            }
        }
    }

    private void normalNoise() throws Exception {
        Method create = names.method("NormalNoise", "NormalNoise#create",
            names.type("RandomSource"), int.class, double[].class);
        Method getValue = names.method("NormalNoise", "NormalNoise#getValue",
            double.class, double.class, double.class);
        Method maxValue = names.method("NormalNoise", "NormalNoise#maxValue");
        Constructor<?> xoro = names.constructor("XoroshiroRandomSource", long.class);

        for (int firstOctave : FIRST_OCTAVES) {
            for (int a = 0; a < AMPLITUDE_SETS.length; a++) {
                Object noise = create.invoke(null, xoro.newInstance(42L), firstOctave,
                    AMPLITUDE_SETS[a].clone());
                String base = "normal/" + firstOctave + "/" + a;
                f64(base + "/max_value", (Double) maxValue.invoke(noise));
                for (double[] s : SAMPLES) {
                    f64(base + "/value/" + at(s), (Double) getValue.invoke(noise, s[0], s[1], s[2]));
                }
            }
        }
    }

    private void blendedNoise() throws Exception {
        Constructor<?> make = names.constructor("BlendedNoise", names.type("RandomSource"),
            double.class, double.class, double.class, double.class, double.class);
        Method compute = names.method("BlendedNoise", "BlendedNoise#compute",
            names.type("FunctionContext"));
        Constructor<?> point = names.constructor("SinglePointContext",
            int.class, int.class, int.class);
        Constructor<?> xoro = names.constructor("XoroshiroRandomSource", long.class);

        double[][] settings = {
            {0.25, 0.125, 80.0, 160.0, 8.0},
            {1.0, 1.0, 80.0, 160.0, 8.0},
            {0.5, 0.5, 20.0, 40.0, 2.0},
        };
        for (int s = 0; s < settings.length; s++) {
            double[] c = settings[s];
            Object noise = make.newInstance(xoro.newInstance(42L), c[0], c[1], c[2], c[3], c[4]);
            for (int[] p : POSITIONS) {
                double value = (Double) compute.invoke(noise, point.newInstance(p[0], p[1], p[2]));
                f64("blended/" + s + "/" + at(p), value);
            }
        }
    }

    // ---- Row naming --------------------------------------------------------

    private static String at(int[] p) {
        return p[0] + "," + p[1] + "," + p[2];
    }

    private static String at(double[] p) {
        return p[0] + "," + p[1] + "," + p[2];
    }

    /** A hashed string rendered so it survives being a row name. */
    private static String label(String text) {
        if (text.isEmpty()) {
            return "(empty)";
        }
        return text.replace('\t', '_').replace('\n', '_').replace(' ', '_');
    }
}
