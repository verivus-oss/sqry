# Safe navigation operator patterns for relation extraction testing

class UserService
  def get_user_email(user)
    user&.email
  end

  def get_user_profile_name(user)
    user&.profile&.name
  end

  def check_permissions(user)
    user&.permissions&.include?("admin")
  end
end

class AccountManager
  def send_notification(account)
    account&.owner&.email&.downcase
  end

  def get_setting(account, key)
    account&.settings&.fetch(key)
  end
end

def chained_safe_navigation(obj)
  obj&.method_one&.method_two&.method_three
end
