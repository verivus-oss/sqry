// P/Invoke (Platform Invoke) for C interop

using System;
using System.Runtime.InteropServices;

namespace Demo.Interop {
    public class NativeMethods {
        [DllImport("user32.dll")]
        public static extern int MessageBox(IntPtr hWnd, string text, string caption, uint type);

        [DllImport("kernel32.dll")]
        public static extern IntPtr LoadLibrary(string dllToLoad);

        [DllImport("kernel32.dll")]
        public static extern IntPtr GetProcAddress(IntPtr hModule, string procedureName);

        [DllImport("kernel32.dll")]
        public static extern bool FreeLibrary(IntPtr hModule);
    }

    public class InteropService {
        public void ShowMessage() {
            NativeMethods.MessageBox(IntPtr.Zero, "Hello", "Title", 0);
        }

        public void LoadNativeLibrary() {
            var handle = NativeMethods.LoadLibrary("mylibrary.dll");
            if (handle != IntPtr.Zero) {
                NativeMethods.FreeLibrary(handle);
            }
        }
    }
}
