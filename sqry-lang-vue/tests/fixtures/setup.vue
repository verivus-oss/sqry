<template>
  <div>
    <h1>{{ count }}</h1>
    <button @click="increment">Increment</button>
  </div>
</template>

<script setup>
import { ref } from 'vue';

const count = ref(0);

function increment() {
  count.value++;
  logIncrement();
}

function logIncrement() {
  console.log('Incremented to:', count.value);
}
</script>
