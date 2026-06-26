-- Hand-written Lua control-flow sample for shape descriptor coverage.

local function classify(value, label, ...)
  local result = 0
  if value > 100 then
    result = 1
  elseif value > 10 then
    result = 2
  else
    result = 3
  end

  while result > 0 do
    result = result - 1
    if result == 2 then
      break
    end
  end

  repeat
    result = result + 1
  until result >= 5

  for i = 1, 3 do
    helper(i)
  end

  local rest = { ... }
  for index, entry in ipairs(rest) do
    helper(entry)
  end

  local double = function(x)
    return x * 2
  end

  local ok, err = pcall(function()
    if value < 0 then
      error("bad")
    end
    helper(value)
  end)

  if not ok then
    result = -1
  end

  return result, double(result)
end

function helper(x)
  return x + 1
end

return classify
