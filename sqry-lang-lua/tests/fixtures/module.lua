local M = {}

local helper = function(value)
  return value
end

function M.add(x, y)
  return x + y
end

function M:scale(factor)
  return self.value * factor
end

M.compute = function(a, b, ...)
  return a + b
end

local json = require("dkjson")
dofile "scripts/init.lua"

return M
