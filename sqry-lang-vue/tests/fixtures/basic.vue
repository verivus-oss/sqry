<template>
  <div class="counter">
    <h1>{{ count }}</h1>
    <button @click="increment">Increment</button>
  </div>
</template>

<script>
export default {
  name: 'Counter',
  data() {
    return {
      count: 0
    };
  },
  methods: {
    increment() {
      this.count++;
      this.logChange();
    },
    logChange() {
      console.log('Count changed to:', this.count);
    }
  }
};
</script>

<style scoped>
.counter {
  text-align: center;
}
</style>
