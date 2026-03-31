module Math where

import Data.List

add :: Int -> Int -> Int
add x y = x + y

factorial :: Integer -> Integer
factorial 0 = 1
factorial n = n * factorial (n - 1)

data Point = Point { x :: Int, y :: Int }

class Addable a where
    plus :: a -> a -> a
