// Test fixture: Native methods (JNI)
// Tests: native method detection

package com.example.jni;

public class NativeMethods {

    // Load native library
    static {
        System.loadLibrary("nativelib");
    }

    // Native method declarations
    public native void nativeVoidMethod();

    public native int nativeIntMethod(int x);

    public native String nativeStringMethod(String input);

    public static native long nativeStaticMethod();

    private native double nativePrivateMethod(double a, double b);

    // Regular Java method that calls native
    public int regularMethod(int x) {
        int result = nativeIntMethod(x);
        return result * 2;
    }

    // Regular method
    public void helperMethod() {
        nativeVoidMethod();
        long value = nativeStaticMethod();
        if (value > 0) {
            nativeVoidMethod();
        }
    }

    public static void main(String[] args) {
        NativeMethods obj = new NativeMethods();
        obj.nativeVoidMethod();
        int result = obj.nativeIntMethod(42);
        String str = obj.nativeStringMethod("test");
        long staticResult = nativeStaticMethod();

        if (result > 0 && str != null && staticResult > 0) {
            obj.regularMethod(result);
        }
    }
}
