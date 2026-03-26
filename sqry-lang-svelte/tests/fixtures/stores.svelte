<script>
  import { writable } from 'svelte/store';
  import { count } from './stores';

  $: doubleCount = $count * 2;
</script>

<div>
  <p>Count: {$count}</p>
  <p>Double: {doubleCount}</p>
</div>
