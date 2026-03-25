module Debug where

foreign import ccall "test" c_test :: Int -> Int
