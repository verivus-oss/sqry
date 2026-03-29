module Math
  class Calculator
    attr_accessor :value

    def initialize
      @value = 0
    end

    def add(a, b)
      a + b
    end

    def self.multiply(a, b)
      a * b
    end

    private

    def private_method
      "private"
    end
  end
end

class Point < Struct.new(:x, :y)
  def distance_from_origin
    Math.sqrt(x**2 + y**2)
  end
end
