<?php
/**
 * Dynamic Call Extraction Fixture
 *
 * Tests dynamic function calls, call_user_func, variable functions, and magic methods.
 * Ground truth annotations mark expected call edges where literals are extractable.
 */

namespace App\Dynamic;

class CallableRouter {
    private $handlers = [];

    public function register($name, callable $handler) {
        $this->handlers[$name] = $handler;
    }

    // CALL: call_user_func_array (dynamic: true)
    public function dispatch($name, $args = []) {
        if (isset($this->handlers[$name])) {
            return call_user_func_array($this->handlers[$name], $args);
        }
        return null;
    }

    // CALL: call_user_func (dynamic: true, target: processData - literal)
    public function handleData($data) {
        // Dynamic call with literal string - can be extracted
        return call_user_func('processData', $data);
    }

    // CALL: call_user_func (dynamic: true, target: App\Helpers\format - literal)
    public function formatData($data) {
        // Dynamic call with literal namespaced function
        return call_user_func('App\\Helpers\\format', $data);
    }

    // Variable function call - can't extract target statically
    public function invokeVariable($functionName, $data) {
        return $functionName($data);
    }
}

class DynamicMethodCaller {
    // CALL: sprintf
    public function callMethod($object, $methodName, $args = []) {
        if (method_exists($object, $methodName)) {
            // Variable method call - can't extract statically
            return $object->$methodName(...$args);
        }
        throw new \Exception(sprintf('Method %s not found', $methodName));
    }

    // CALL: call_user_func (dynamic: true)
    public function callStatic($className, $methodName, $args = []) {
        return call_user_func([$className, $methodName], ...$args);
    }

    // CALL: call_user_func (dynamic: true, target: UserService::validate - literal)
    public function validateUser($user) {
        // Dynamic call with literal class and method
        return call_user_func(['UserService', 'validate'], $user);
    }
}

// CALL: strtoupper
function processData($data) {
    return strtoupper($data);
}

namespace App\Helpers;

// CALL: json_encode
function format($data) {
    return json_encode($data, JSON_PRETTY_PRINT);
}

namespace App\Magic;

class MagicHandler {
    private $data = [];

    // Magic method - dynamic dispatch
    // CALL: array_key_exists
    // CALL: sprintf
    public function __call($name, $arguments) {
        if (array_key_exists($name, $this->data)) {
            return $this->data[$name];
        }
        throw new \Exception(sprintf('Unknown method: %s', $name));
    }

    // CALL: array_key_exists
    // CALL: sprintf
    public static function __callStatic($name, $arguments) {
        if (array_key_exists($name, self::$staticData)) {
            return self::$staticData[$name];
        }
        throw new \Exception(sprintf('Unknown static method: %s', $name));
    }

    private static $staticData = [];
}

class ReflectionCaller {
    // CALL: ReflectionClass::__construct
    // CALL: ReflectionClass::getMethod
    // CALL: ReflectionMethod::invoke
    public function callReflectively($className, $methodName, $object, $args = []) {
        $reflection = new \ReflectionClass($className);
        $method = $reflection->getMethod($methodName);
        return $method->invoke($object, ...$args);
    }

    // CALL: ReflectionFunction::__construct
    // CALL: ReflectionFunction::invoke
    public function callFunctionReflectively($functionName, $args = []) {
        $reflection = new \ReflectionFunction($functionName);
        return $reflection->invoke(...$args);
    }
}

class UserService {
    // CALL: strlen
    public static function validate($user) {
        return strlen($user) > 0;
    }
}
