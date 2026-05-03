class Ledger
  # @return [Integer]
  attr_accessor :mutable_field
  # @return [Integer]
  attr_reader :immutable_field
  attr_writer :write_only_field
  attr_accessor :shared_name

  private

  attr_accessor :private_field

  protected

  attr_reader :protected_field

  public

  def initialize
    @immutable_field = 1
  end
end

class Archive
  attr_accessor :shared_name
end
