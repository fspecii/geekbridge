// SPDX-License-Identifier: GPL-3.0-or-later
// EXPERIMENT (docs/reports/call-audio-over-wifi-feasibility.md): a tiny helper run as the shell uid through
// `app_process` (see run.sh). It touches hidden Android APIs by reflection only, so it compiles against the public
// android.jar. Nothing here is shipped in the app. Modes:
//   capture SOURCE [SECONDS]        read an AudioRecord source (voice-downlink|voice-uplink|voice-call|mic|voice-comm)
//                                   and print "RESULT source=.. samples=.. rms=.. peak=.." (or the exception class)
//   inject  SOURCE [SECONDS] [HZ]   register an AudioPolicy with an injector AudioMix on the capture presets, play a
//                                   sine tone into it, record SOURCE meanwhile and print the level of that tone in it
// The technique (AudioPolicy + AudioMix of role injector on capture presets, API 33+) is the one discussed in
// Genymobile/scrcpy issue 3880 and PRs 6439/7004; this is an independent, much smaller reimplementation.
import android.app.Application;
import android.content.AttributionSource;
import android.content.Context;
import android.content.ContextWrapper;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaRecorder;
import android.content.pm.ApplicationInfo;
import android.os.Looper;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;

public final class ShellProbe {
    static final String PKG = "com.android.shell";

    static final class Fake extends ContextWrapper {
        Fake(Context base) { super(base); }
        @Override public String getPackageName() { return PKG; }
        @Override public String getOpPackageName() { return PKG; }
        @Override public String getAttributionTag() { return null; }
        @Override public Context getApplicationContext() { return this; }
        @Override public AttributionSource getAttributionSource() {
            return new AttributionSource.Builder(2000).setPackageName(PKG).build();
        }
        @Override public int getDeviceId() { return 0; }
        @Override public Looper getMainLooper() { return Looper.getMainLooper(); }
    }

    static Context fillApp() throws Exception {
        Class<?> atc = Class.forName("android.app.ActivityThread");
        Constructor<?> c = atc.getDeclaredConstructor();
        c.setAccessible(true);
        Object at = c.newInstance();
        set(atc, "sCurrentActivityThread", null, at);
        set(atc, "mSystemThread", at, true);
        Class<?> abd = Class.forName("android.app.ActivityThread$AppBindData");
        Constructor<?> ac = abd.getDeclaredConstructor();
        ac.setAccessible(true);
        Object data = ac.newInstance();
        ApplicationInfo ai = new ApplicationInfo();
        ai.packageName = PKG;
        set(abd, "appInfo", data, ai);
        set(atc, "mBoundApplication", at, data);
        Method sys = Class.forName("android.app.ContextImpl").getDeclaredMethod("createSystemContext", atc);
        sys.setAccessible(true);
        Fake fake = new Fake((Context) sys.invoke(null, at));
        Application app = new Application();
        Method attach = ContextWrapper.class.getDeclaredMethod("attachBaseContext", Context.class);
        attach.setAccessible(true);
        attach.invoke(app, fake);
        set(atc, "mInitialApplication", at, app);
        return fake;
    }

    static void set(Class<?> k, String name, Object target, Object v) throws Exception {
        Field f = k.getDeclaredField(name);
        f.setAccessible(true);
        f.set(target, v);
    }

    static int source(String s) {
        switch (s) {
            case "voice-downlink": return MediaRecorder.AudioSource.VOICE_DOWNLINK;
            case "voice-uplink": return MediaRecorder.AudioSource.VOICE_UPLINK;
            case "voice-call": return MediaRecorder.AudioSource.VOICE_CALL;
            case "voice-comm": return MediaRecorder.AudioSource.VOICE_COMMUNICATION;
            default: return MediaRecorder.AudioSource.MIC;
        }
    }

    /** Reads [ms] of [src]; returns {samples, rms, peak, toneMagnitude(hz) or -1}. Throws what Android throws. */
    static double[] record(int src, int ms, double hz) throws Exception {
        int rate = 16000;
        AudioFormat af = new AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(rate).setChannelMask(AudioFormat.CHANNEL_IN_MONO).build();
        AudioRecord r = new AudioRecord.Builder().setAudioSource(src).setAudioFormat(af)
            .setBufferSizeInBytes(rate * 2).build();
        if (r.getState() != AudioRecord.STATE_INITIALIZED) {
            r.release();
            throw new IllegalStateException("AudioRecord not initialised");
        }
        short[] buf = new short[rate / 10];
        long n = 0, peak = 0;
        double sq = 0, gc = 0, gs = 0;
        r.startRecording();
        long end = System.nanoTime() + ms * 1_000_000L;
        try {
            while (System.nanoTime() < end) {
                int got = r.read(buf, 0, buf.length);
                if (got < 0) throw new IllegalStateException("read error " + got);
                for (int i = 0; i < got; i++) {
                    double v = buf[i];
                    sq += v * v;
                    peak = Math.max(peak, Math.abs(buf[i]));
                    double ph = 2 * Math.PI * hz * (n + i) / rate;
                    gc += v * Math.cos(ph);
                    gs += v * Math.sin(ph);
                }
                n += got;
            }
        } finally {
            try { r.stop(); } catch (RuntimeException ignored) { }
            r.release();
        }
        double rms = n == 0 ? 0 : Math.sqrt(sq / n);
        double tone = n == 0 ? 0 : 2 * Math.sqrt(gc * gc + gs * gs) / n;
        return new double[] {n, rms, peak, tone};
    }

    static String describe(String name, double[] x) {
        return String.format("RESULT source=%s samples=%d rms=%.1f peak=%d tone=%.1f", name, (long) x[0], x[1], (long) x[2], x[3]);
    }

    /** Sets up the injector policy; returns the tone-playing thread (already started). */
    static Thread inject(Context ctx, double hz, int ms, String[] diag) throws Exception {
        Class<?> ruleB = Class.forName("android.media.audiopolicy.AudioMixingRule$Builder");
        Class<?> rule = Class.forName("android.media.audiopolicy.AudioMixingRule");
        Object rb = ruleB.getConstructor().newInstance();
        int match = rule.getField("RULE_MATCH_ATTRIBUTE_CAPTURE_PRESET").getInt(null);
        Method add = ruleB.getMethod("addMixRule", int.class, Object.class);
        Method preset = AudioAttributes.Builder.class.getMethod("setInternalCapturePreset", int.class);
        int[] presets = {0, 1, 5, 6, 7, 9, 10}; // DEFAULT MIC CAMCORDER VOICE_RECOGNITION VOICE_COMMUNICATION UNPROCESSED VOICE_PERFORMANCE
        for (int p : presets) {
            AudioAttributes.Builder ab = new AudioAttributes.Builder();
            preset.invoke(ab, p);
            add.invoke(rb, match, ab.build());
        }
        int injector = rule.getField("MIX_ROLE_INJECTOR").getInt(null);
        ruleB.getMethod("setTargetMixRole", int.class).invoke(rb, injector);
        Object mixRule = ruleB.getMethod("build").invoke(rb);

        int rate = 48000;
        AudioFormat mixFormat = new AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT)
            .setSampleRate(rate).setChannelMask(AudioFormat.CHANNEL_IN_STEREO).build();
        Class<?> mixB = Class.forName("android.media.audiopolicy.AudioMix$Builder");
        Class<?> mixC = Class.forName("android.media.audiopolicy.AudioMix");
        Object mb = mixB.getConstructor(rule).newInstance(mixRule);
        mixB.getMethod("setFormat", AudioFormat.class).invoke(mb, mixFormat);
        mixB.getMethod("setRouteFlags", int.class).invoke(mb, mixC.getField("ROUTE_FLAG_LOOP_BACK").getInt(null));
        Object mix = mixB.getMethod("build").invoke(mb);

        Class<?> polB = Class.forName("android.media.audiopolicy.AudioPolicy$Builder");
        Class<?> polC = Class.forName("android.media.audiopolicy.AudioPolicy");
        Object pb = polB.getConstructor(Context.class).newInstance(ctx);
        polB.getMethod("addMix", mixC).invoke(pb, mix);
        Object policy = polB.getMethod("build").invoke(pb);
        AudioManager am = AudioManager.class.getConstructor(Context.class).newInstance(ctx);
        int rc = (Integer) AudioManager.class.getMethod("registerAudioPolicy", polC).invoke(am, policy);
        diag[0] = "registerAudioPolicy rc=" + rc + " (0 = success)";
        if (rc != 0) throw new IllegalStateException(diag[0]);
        AudioTrack track = (AudioTrack) polC.getMethod("createAudioTrackSource", mixC).invoke(policy, mix);
        if (track.getState() != AudioTrack.STATE_INITIALIZED) throw new IllegalStateException("injector AudioTrack not initialised");
        track.play();
        Thread t = new Thread(() -> {
            short[] b = new short[rate / 50 * 2];
            long n = 0;
            // Quiet (about -17 dBFS) with a 10 ms fade in and out: this may reach a real call's recorders.
            final double amp = 0.14 * 32767;
            final double fade = rate / 100.0;
            final long total = (long) (ms + 1500) * rate / 1000;
            while (n < total) {
                for (int i = 0; i < b.length; i += 2) {
                    double g = n >= total ? 0 : Math.min(1.0, Math.min(n / fade, (total - 1 - n) / fade));
                    short v = (short) (amp * g * Math.sin(2 * Math.PI * hz * n / rate));
                    n++;
                    b[i] = v; b[i + 1] = v;
                }
                if (track.write(b, 0, b.length) < 0) break;
            }
            track.stop();
            track.release();
        }, "inject-tone");
        t.start();
        return t;
    }

    public static void main(String[] args) throws Exception {
        Looper.prepareMainLooper();
        Context ctx = fillApp();
        Thread worker = new Thread(() -> {
            int code = 0;
            try {
                String mode = args.length > 0 ? args[0] : "capture";
                String src = args.length > 1 ? args[1] : "voice-downlink";
                int secs = args.length > 2 ? Integer.parseInt(args[2]) : 5;
                double hz = args.length > 3 ? Double.parseDouble(args[3]) : 1000;
                if (mode.equals("inject")) {
                    String[] diag = new String[1];
                    Thread t = inject(ctx, hz, secs * 1000, diag);
                    System.out.println("INFO " + diag[0]);
                    Thread.sleep(500);
                    System.out.println(describe(src + "+injected-" + (int) hz + "Hz", record(source(src), secs * 1000, hz)));
                    t.join();
                } else {
                    System.out.println(describe(src, record(source(src), secs * 1000, hz)));
                }
            } catch (Throwable e) {
                Throwable c = e instanceof java.lang.reflect.InvocationTargetException && e.getCause() != null ? e.getCause() : e;
                e.printStackTrace(System.out);
                System.out.println("RESULT error=" + c.getClass().getSimpleName() + " msg=" + c.getMessage());
                code = 1;
            }
            System.out.flush();
            System.exit(code);
        }, "probe");
        worker.start();
        Looper.loop();
    }
}
