{-# LANGUAGE ForeignFunctionInterface #-}

module FFIExample where

import Foreign.C.Types
import Foreign.Ptr

-- Pattern 1: Static FFI call (ccall)
foreign import ccall "exp" c_exp :: Double -> Double
foreign import ccall "strlen" c_strlen :: CString -> IO CSize

-- Pattern 2: Dynamic FFI call
foreign import ccall "dynamic" mkCallback :: FunPtr (CInt -> IO ()) -> (CInt -> IO ())

-- Pattern 3: Address-of operator
foreign import ccall "&errno" errno_ptr :: Ptr CInt
foreign import ccall "&stdout" stdout_ptr :: Ptr CFile

-- Pattern 4: Wrapper (Haskell → C)
foreign import ccall "wrapper" createCallback :: (CInt -> IO ()) -> IO (FunPtr (CInt -> IO ()))

-- Pattern 5: Stdcall convention
foreign import stdcall "MessageBoxA" messageBox :: Ptr () -> CString -> CString -> CInt -> IO CInt

-- Pattern 6: CAPI
foreign import capi "stdio.h printf" my_printf :: CString -> IO CInt

-- Safety modifiers
foreign import ccall unsafe "fast_func" fast :: CInt -> CInt
foreign import ccall safe "blocking_func" blocking :: CInt -> IO CInt

-- Regular Haskell function (should NOT create FFI edge)
regularFunction :: Int -> Int
regularFunction x = x + 1

-- Use one of the FFI functions
testExp :: Double -> Double
testExp = c_exp
