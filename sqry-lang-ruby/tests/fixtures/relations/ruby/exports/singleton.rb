# Singleton method (class method) patterns for export extraction testing

class DatabaseConnection
  def self.connect(host)
    "Connecting to #{host}"
  end

  def self.disconnect
    "Disconnecting"
  end

  def instance_method
    "Instance level"
  end
end

class ConfigManager
  class << self
    def load_config
      "Loading"
    end

    def save_config
      "Saving"
    end
  end

  def self.reset
    "Resetting"
  end
end

class Logger
  def self.info(message)
    puts message
  end

  def self.error(message)
    warn message
  end

  def log(message)
    puts message
  end
end

module FactoryMethods
  def self.create_user(name)
    "Creating user: #{name}"
  end

  def self.create_admin(name)
    "Creating admin: #{name}"
  end
end
