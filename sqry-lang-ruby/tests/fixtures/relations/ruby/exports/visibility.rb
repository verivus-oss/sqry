# Visibility modifier patterns for export extraction testing

# Pattern 1: Scope markers (private/protected/public as standalone statements)
class ServiceWithScopeMarkers
  def public_method_one
    "public"
  end

  def public_method_two
    "public"
  end

  private

  def private_method_one
    "private"
  end

  def private_method_two
    "private"
  end

  protected

  def protected_method
    "protected"
  end

  public

  def public_again
    "public"
  end
end

# Pattern 2: Inline modifiers (private def method_name)
class ServiceWithInlineModifiers
  def public_method
    "public"
  end

  private def inline_private
    "private"
  end

  protected def inline_protected
    "protected"
  end

  public def explicit_public
    "public"
  end
end

# Pattern 3: Post-declaration (private :method_name)
class ServiceWithPostDeclaration
  def helper
    "should be private"
  end
  private :helper

  def utility
    "should be private"
  end

  def public_api
    "should be public"
  end

  private :utility

  def another_public
    "should be public"
  end
end

# Combined patterns
class MixedVisibility
  def api_method
    "public"
  end

  private def secret_one
    "private inline"
  end

  private

  def secret_two
    "private scope"
  end

  public

  def public_helper
    "public"
  end

  private :public_helper  # Now private via post-declaration
end
