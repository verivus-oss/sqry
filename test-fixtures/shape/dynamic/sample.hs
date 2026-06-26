-- Hand-written Haskell control-flow sample for shape descriptor coverage.
module Sample where

classify :: Int -> String -> [Int] -> (String, [Int])
classify value label rest =
  let result
        | value > 100 = 1
        | value > 10 = 2
        | otherwise = 3

      bucket = case value of
        0 -> "zero"
        n | n >= 1 && n <= 9 -> "small"
        _ -> "large"

      doubled = [x * 2 | x <- rest, x > 0]

      mapped = map (\x -> x + result) rest
  in
    if value < 0
      then (bucket, [])
      else (bucket, doubled ++ mapped)

helper :: Int -> Int
helper x = x + 1

run :: Int -> IO ()
run value = do
  let r = helper value
  putStrLn (show r)
  case r of
    0 -> putStrLn "zero"
    _ -> putStrLn "other"
