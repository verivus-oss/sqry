module Negative where

-- Regular import (not FFI)
import Data.List

-- Regular function
notFfi :: Int -> Int
notFfi x = x * 2

-- Another regular function
anotherFunc :: String -> String
anotherFunc s = s ++ "!"

-- "foreign" in comment should not trigger detection
-- foreign import ccall "fake" fake :: Int -> Int

-- "foreign" in string
message :: String
message = "This is not a foreign import"

-- Regular type definition
data MyType = MyConstructor Int

-- Regular class
class MyClass a where
  myMethod :: a -> a
