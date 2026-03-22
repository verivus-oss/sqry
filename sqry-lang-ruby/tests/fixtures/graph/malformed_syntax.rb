# Intentionally malformed Ruby for error handling tests
class BrokenClass
  def incomplete_method
    # Missing 'end' - syntax error

  def another_method
    "this should not be parsed"
  end
end

# Unclosed string
invalid_string = "this string never closes

# Invalid method definition
def (broken receiver).method_name
end
