# R8 keep rules for the release build (docs/android-perf.md).

# JNI: the Rust core exports Java_dev_geekbridge_android_NativeBridge_nativeRequest; renaming the class or
# the method would make System.loadLibrary succeed and every call fail.
-keep class dev.geekbridge.android.NativeBridge { native <methods>; }

# QuickActions.load() finds its handlers by name with Class.forName(...).getField("INSTANCE").
-keep class dev.geekbridge.android.*Action { public static final ** INSTANCE; }

# Kotlin null-check intrinsics on parameters and on platform-typed expressions (about 1 500 call sites, each a string constant and an
# invoke). They only fire when Java/framework code hands null to a non-null Kotlin parameter; without them the same null throws a
# little later at the first use. Release only, debug and unit tests keep the checks. Intrinsics.checkNotNull (the `!!` operator)
# and lateinit checks are not listed and stay.
-assumenosideeffects class kotlin.jvm.internal.Intrinsics {
    public static void checkNotNullParameter(java.lang.Object, java.lang.String);
    public static void checkNotNullExpressionValue(java.lang.Object, java.lang.String);
    public static void checkParameterIsNotNull(java.lang.Object, java.lang.String);
    public static void checkExpressionValueIsNotNull(java.lang.Object, java.lang.String);
}
