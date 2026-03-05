# Test fixture for qualified callee name resolution
# RKG: TEST:RUBY-CALLEE-RESOLUTION tests REQ:SQRY-RUBY-QUALIFIED-CALLERS

module Database
  class Connection
    def self.execute(sql)
      puts sql
    end

    def self.query(sql)
      puts sql
    end
  end
end

module Admin
  module Users
    class Controller
      def show
        # Qualified callee: Database::Connection.execute
        Database::Connection.execute("SELECT * FROM users")

        # Another qualified call
        Database::Connection.query("SELECT id FROM users")
      end

      def update
        # Qualified callee with absolute path
        ::Database::Connection.execute("UPDATE users SET name = 'test'")
      end
    end
  end
end

class Application
  def bootstrap
    # Qualified callees in global scope
    Database::Connection.execute("CREATE TABLE users")
    Database::Connection.query("SELECT 1")
  end
end

# Top-level qualified calls
Database::Connection.execute("PRAGMA foreign_keys = ON")
