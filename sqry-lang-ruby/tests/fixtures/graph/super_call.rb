class Base
  def foo
    # Base implementation - intentionally empty for super call testing
  end
end

class Child < Base
  def foo
    super
  end
end

