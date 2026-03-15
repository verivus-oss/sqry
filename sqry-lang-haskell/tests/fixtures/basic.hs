module Sample (foo, Bar(..), Run(..)) where

import qualified Data.List as List
import Data.List (sort)
import Control.Monad (when)

data Bar a = Bar a | Baz
newtype Wrapped = Wrapped Int
type Alias = Int

class Run a where
  run :: a -> Int

instance Run Int where
  run x = x

foo :: Int -> Int
foo x = x + 1

pattern Empty <- []

bar = 42
