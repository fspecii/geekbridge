# R8 keep rules for the release build (docs/android-perf.md).

# JNI: the Rust core exports Java_dev_geekbridge_android_NativeBridge_nativeRequest; renaming the class or
# the method would make System.loadLibrary succeed and every call fail.
-keep class dev.geekbridge.android.NativeBridge { native <methods>; }

# QuickActions.load() finds its handlers by name with Class.forName(...).getField("INSTANCE").
-keep class dev.geekbridge.android.*Action { public static final ** INSTANCE; }
