# SPDX-License-Identifier: MIT
class SessionRecord
  attr_accessor :mutable_field
  attr_reader :immutable_field
  attr_accessor :shared_name

  def initialize(value)
    @mutable_field = value
    @immutable_field = "fixed"
  end
end

class AuditRecord
  attr_accessor :shared_name
end
