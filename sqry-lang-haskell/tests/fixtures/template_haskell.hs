module WithTH where

$(generateSomething)

foo x = $(embed [d| bar = x |])
