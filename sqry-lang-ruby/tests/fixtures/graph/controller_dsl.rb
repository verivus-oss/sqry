class UsersController
  before_action :require_login, only: [:new, :create]

  def new
    # action method
  end

  def create
    # action method
  end

  def show
    # not filtered - should have no edge
  end

  def require_login
    # callback method - should be target of edges from new and create
  end
end
