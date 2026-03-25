import 'dart:ffi' as ffi;

// Pattern 1-3: DynamicLibrary loading
void loadLibraries() {
  final lib1 = ffi.DynamicLibrary.open('libhello.so');
  final lib2 = ffi.DynamicLibrary.executable();
  final lib3 = ffi.DynamicLibrary.process();
}

// Pattern 4: lookup().asFunction()
void useLookupChain() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
  final hello = dylib
      .lookup<ffi.NativeFunction<ffi.Int32 Function(ffi.Int32)>>('hello')
      .asFunction<int Function(int)>();
  hello(42);
}

// Pattern 5: lookupFunction()
void useLookupFunction() {
  final dylib = ffi.DynamicLibrary.open('libhello.so');
  final hello = dylib.lookupFunction<
      ffi.NativeFunction<ffi.Int32 Function(ffi.Int32)>,
      int Function(int)>('hello');
  hello(42);
}

// Pattern 6: @Native annotation with explicit symbol (Dart 3.0+)
@ffi.Native<ffi.Int32 Function(ffi.Int32)>(symbol: 'add')
external int nativeAdd(int a, int b);

// Pattern 7: @FfiNative annotation (pre-Dart 3.0)
@ffi.FfiNative<ffi.Int32 Function(ffi.Int32)>('multiply')
external int nativeMultiply(int a, int b);

void main() {
  loadLibraries();
  useLookupChain();
  useLookupFunction();
  print('Add: ${nativeAdd(2, 3)}');
  print('Multiply: ${nativeMultiply(4, 5)}');
}
