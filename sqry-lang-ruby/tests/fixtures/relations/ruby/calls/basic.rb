# Basic method call patterns for relation extraction testing

class Calculator
  def add(a, b)
    sum(a, b)
  end

  def multiply(x, y)
    product(x, y)
  end

  def compute
    add(5, 10)
    multiply(2, 3)
  end
end

class Logger
  def log_info(message)
    puts message
  end

  def log_error(error)
    warn error
  end
end

def standalone_function
  puts "hello"
  print "world"
end
