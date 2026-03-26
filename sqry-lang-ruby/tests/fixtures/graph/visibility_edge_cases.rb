# Test visibility modifier edge cases
class VisibilityTest
  # Standard visibility
  def public_method
    "public"
  end

  private

  def private_method_1
    "private 1"
  end

  def private_method_2
    "private 2"
  end

  # Inline visibility with method name (edge case)
  private :private_method_1, :private_method_2

  # Reset to public
  public

  def another_public
    private_method_1
    private_method_2
  end

  # Protected methods
  protected

  def protected_method
    "protected"
  end

  # Visibility with arguments (should not change default)
  private def inline_private
    "inline private"
  end

  # Class methods with visibility
  class << self
    def public_class_method
      "public singleton"
    end

    private

    def private_class_method
      "private singleton"
    end
  end

  # Using visibility modifiers as method calls with arguments
  def regular_method
    "regular"
  end

  private :regular_method
end

# Module with mixed visibility
module MixedVisibility
  def module_public
    "public"
  end

  private

  def module_private
    "private"
  end

  module_function :module_public
end
