<script lang="ts">
  function calculate(x: number, y: number): number {
    return add(x, y);
  }

  function add(a: number, b: number): number {
    return a + b;
  }

  interface User {
    name: string;
    age: number;
  }

  function createUser(name: string, age: number): User {
    return { name, age };
  }
</script>

<div>TypeScript component</div>
