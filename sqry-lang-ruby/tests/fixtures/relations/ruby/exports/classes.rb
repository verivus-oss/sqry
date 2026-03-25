# Class and module definition patterns for export extraction testing

class User
  def initialize(name)
    @name = name
  end

  def greet
    "Hello"
  end
end

class Admin < User
  def manage
    "Managing"
  end
end

module Helpers
  def helper_method
    "Help"
  end
end

module Validators
  def validate_email(email)
    email.include?("@")
  end

  def validate_presence(value)
    !value.nil? && !value.empty?
  end
end

class ServiceWithModule
  include Helpers

  def process
    helper_method
  end
end
