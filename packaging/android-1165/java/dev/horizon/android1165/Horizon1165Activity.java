package dev.horizon.android1165;

/**
 * NativeActivity shell for the native 1.16.5 target.
 *
 * <p>winit's {@code android-native-activity} support exports {@code android_main}
 * from the Rust cdylib; this class only names the library and keeps the Java
 * component required by the manifest.
 */
public final class Horizon1165Activity extends android.app.NativeActivity {
    static {
        System.loadLibrary("horizon_1_16_5");
    }
}
