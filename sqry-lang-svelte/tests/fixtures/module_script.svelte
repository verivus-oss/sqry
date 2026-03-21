<script context="module">
  export function moduleHelper() {
    return "shared data";
  }

  function internalModuleFunc() {
    return moduleHelper();
  }
</script>

<script>
  function instanceFunc() {
    const data = moduleHelper();
    return processData(data);
  }

  function processData(value) {
    return value.toUpperCase();
  }
</script>

<div>Module and instance scripts</div>
