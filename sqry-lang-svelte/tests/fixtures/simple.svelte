<script>
  export let name = 'world';

  let count = 0;

  $: doubled = count * 2;

  function increment() {
    count += 1;
  }
</script>

<div>
  <h1>Hello {name}!</h1>
  <p>Count: {count}</p>
  <p>Doubled: {doubled}</p>
  <button on:click={increment}>Increment</button>
</div>

<style>
  div {
    padding: 20px;
  }
</style>
