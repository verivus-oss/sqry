require 'active_support'
require_relative '../lib/my_app'
load 'legacy/tasks'

include MyApp::Support::Helpers
extend CoreExtensions
prepend Audit::Hooks

autoload :Widget, 'my_app/widget'
autoload 'Service', 'my_app/service'
