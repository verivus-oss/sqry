module Mixins
  module Auditable
    def log_action
      audit_event()
    end
  end
end

module Users
  class Controller
  end
end

module Admin
  class Users::Controller
    include Mixins::Auditable

    def show
      render_view()
      log_action()
    end

    class << self
      def build
        new()
      end
    end

    def self.authenticate
      verify_password()
    end
  end
end

class Dashboard
  include Mixins::Auditable

  def render
    log_action()
  end
end
