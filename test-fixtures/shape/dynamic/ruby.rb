# Hand-written Ruby control-flow sample for shape descriptor coverage.

def classify(value, label: "n/a", *rest, **opts)
  result = 0
  if value > 100
    result = 1
  elsif value > 10
    result = 2
  else
    result = 3
  end

  case value
  when 0
    return :zero
  when 1..9
    return :small
  else
    :large
  end

  while result > 0
    result -= 1
    next if result == 2
    break if result < 0
  end

  for item in rest
    puts item
  end

  rest.each do |entry|
    yield entry if block_given?
  end

  mapped = rest.map { |x| x * 2 }

  begin
    raise ArgumentError, "bad" if value.nil?
    helper(value)
  rescue ArgumentError => e
    retry if opts[:retry]
  ensure
    cleanup
  end

  mapped
end

def helper(x)
  x + 1
end

def cleanup
  nil
end
