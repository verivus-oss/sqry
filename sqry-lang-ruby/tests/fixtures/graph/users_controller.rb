class UsersController
  def create
    user = User.new
    user.save
    send_welcome_email
  end

  def send_welcome_email
    Mailer.deliver
  end

  def self.log(message)
    Logger.info(message)
  end

  def self.audit(user)
    log("audit: #{user.id}")
  end
end
