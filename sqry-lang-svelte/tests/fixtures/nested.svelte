<script>
  function outer() {
    function inner() {
      return helper();
    }
    return inner();
  }

  function helper() {
    return deepHelper();
  }

  function deepHelper() {
    return 42;
  }
</script>

<div>Nested function calls</div>
