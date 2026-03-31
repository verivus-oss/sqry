local module = {}

function module.add(a, b)
    return a + b
end

local function private_func(x)
    return x * 2
end

function module:method(value)
    return self.data + value
end

return module
