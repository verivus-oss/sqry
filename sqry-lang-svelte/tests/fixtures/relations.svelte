<script lang="ts">
  import { fade } from 'svelte/transition';
  import { derived, writable } from 'svelte/store';

  export let items: string[] = [];

  const count = writable(0);
  export const userStore = writable({ name: 'Casey', role: 'admin' });

  $: total = items.length;
  $: doubled = $count * 2;

  const filtered = derived([userStore, count], ([$userStore, $count]) =>
    items.filter((item) => item.includes($userStore.name.slice(0, $count + 1))),
  );

  let selected = '';

  function selectItem(value: string) {
    selected = value;
    count.update((n) => n + 1);
  }

  function logAction(node: HTMLElement) {
    node.addEventListener('focus', handleFocus);
    return {
      destroy() {
        node.removeEventListener('focus', handleFocus);
      },
    };
  }

  function handleFocus() {
    console.log('Focused');
  }
</script>

<input
  bind:value={selected}
  placeholder="Filter items"
/>

<ul>
  {#each $filtered as item (item)}
    <li
      class:selected={item === selected}
      on:click={() => selectItem(item)}
      use:logAction
      transition:fade>
      {item} — {$userStore.name}
    </li>
  {/each}
</ul>

<p>
  Items: {total} | Doubled Count: {doubled}
</p>
