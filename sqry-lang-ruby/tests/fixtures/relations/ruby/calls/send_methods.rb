# Dynamic method invocation patterns (send, public_send, __send__)

class DynamicCaller
  def call_with_send(obj)
    obj.send(:process)
    obj.send(:validate, "arg1")
  end

  def call_with_public_send(obj)
    obj.public_send(:execute)
    obj.public_send(:transform, data)
  end

  def call_with_double_underscore(obj)
    obj.__send__(:internal_method)
  end
end

class MetaprogrammingExample
  def dynamic_dispatch(method_name, target)
    target.send(method_name)
  end

  def safe_dynamic_call(obj, method_sym)
    obj.public_send(method_sym) if obj.respond_to?(method_sym)
  end
end

def send_with_string(obj)
  obj.send("method_as_string")
end
